// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "Sparkle words": render-time decorations on whole words the lexicon
//! matches.
//!
//! * **Profanity**: rainbow ink with a per-appearance chance of a supernova
//!   (or the classic nova / randomized sparkle styles).
//! * **Feline** and the animal roster: a head that peeks out above (or below)
//!   the word, dwells and descends, once per occurrence; when it cannot draw,
//!   only the word's ink plays.
//! * **Ink**: matched glyphs recolored through [`InkCell`] fg overrides that
//!   settle to constant bytes.
//!
//! Everything is seeded-deterministic and self-terminating. The engine scans
//! the visible grid when the damage epoch advances (into [`Occurrence`]s) and
//! each frame emits [`WordDecoration`]s, [`InkCell`]s, free sprites and nova
//! quads into buffers the renderer composites. Grid cells, copied text and
//! recordings are never touched.

use aterm_time::Instant;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_hash::{FNV64_OFFSET, FxHashMap, KeyedHashMap, fnv_mix_u64, fnv1a_64};
use aterm_lexicon::{Class, FormId, LangSet, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, InkCell, WordDecoration};
use aterm_scene::vector::FIXED_ONE;
use aterm_scene::{mix_rgb, smoothstep};

use crate::animal_baker::{AnimalBakeKey, AnimalBaker};
use crate::animal_glyphs_gen::{ANIMAL_GLYPHS, AnimalGlyphId, animal_glyph_from_key};
use crate::cat_baker::{BakeKeyV4, CatBaker, CatColorKey, CatTile, EyesFrame, PATCH_STRIP};
use crate::kitty_registry::{
    KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
};
// Color math lives in the leaf `color_math` (so `nova.rs` builds without this
// module); imported, not re-exported.
use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphDef};
use crate::color_math::{hsv2rgb, hue_nudge, relative_luminance};
use crate::genome::{
    self, CatAge, Genome, NovaFeatures, NovaMagic, VoteScratch, accessory_variant_v4, cat_age_v4,
    cat_fills_v4, cat_magic, cat_variant_v4, mix, nova_duration_ms, nova_features, nova_magic,
    nova_palette, nova_radius, special_variant_v4,
};
use crate::nova::{self, NovaEnv};
use crate::spec::{
    BurstKind, BurstSpec, Collection, Colorway, GraphicSpec, InkSpec, SpecTable, WordEffectSpec,
};
use crate::supernova::{self, SuperEnv};
use crate::trick_flash::TrickFlash;
// The flash's lifecycle observable is public here (the state lives on
// `WordDecorations`); its defining module is crate-private.
pub use crate::trick_flash::TrickFlashPhase;

/// Cap on occurrences tracked per frame. Truncation drops whole rows top-down
/// (the bottom-priority cutoff restart), so the prompt row keeps its slots
/// even under dense output (see `dense_scan_cutoff`).
const MAX_OCCURRENCES: usize = 128;
/// Hard cap on emitted decorations per frame.
const MAX_DECORATIONS: usize = 256;
/// Held departing-body tiles ([`WordDecorations::pet_depart_tiles`]): twice
/// the pet's departure lane.
const PET_DEPART_HOLDS: usize = 2 * crate::kitty_pet::PET_DEPARTURES_MAX;
/// Cap on emitted ink cells per frame (§4.4): row-major truncation at rescan,
/// whole words only — an occurrence past the cap gets no ink (partial-word ink
/// would sweep a torn gradient).
const MAX_INK_CELLS: usize = 512;
/// Specular sweep ramp-in (ms) — §4.2 `env(t)`.
pub(crate) const INK_RAMP_IN_MS: u64 = 130;
/// Specular sweep ramp-out after `sweep_ms` (ms) — §4.2: after `sweep_ms + 250`
/// the emitted color is exactly the static gradient, constant bytes forever.
pub(crate) const INK_FADE_MS: u64 = 250;
/// §4.3 legibility bound: the word's mid-gradient ink must hold at least this
/// WCAG contrast against the cell background, else the mix is pulled toward
/// the captured base fg until it does. Keeps pastel anchors legible on light
/// themes; dark themes never bind.
pub(crate) const MIN_INK_CONTRAST: f32 = 3.5;
/// Feline ink is a subtle self-terminating glow pulse in the word's own fg
/// color (not a tint); total window, ms.
const FELINE_GLOW_MS: u64 = 1400;
/// The glow's peak "lit" tone: the fg mixed this far toward white (never pure
/// white — the word glows in its own color).
const FELINE_GLOW_LIFT: f32 = 0.72;
/// Peak glow amplitude — subtle; the word brightens softly, it does not flash.
const FELINE_GLOW_AMP: f32 = 0.55;
/// §3.6 persist-map cap: identity episodes tracked across occlusion. Inserting
/// past the cap evicts the oldest-`last_seen` entry (ident tiebreak).
const PERSIST_CAP: usize = 512;
/// §3.6 grace TTL: an identity unseen for longer is dropped at rescan end.
/// Within it, a word occluded by a clear+redraw or brief overwrite keeps its
/// `appeared`, frozen genome and spent nova (`nova_done`), so print-erase-print
/// loops cannot re-roll or strobe (modeled as `SparkleIdentity` in aterm-spec).
const GRACE_TTL: Duration = Duration::from_secs(10);
/// Weak redraw-continuity joins only an immediately adjacent clear/redraw
/// (`visible -> blank -> redrawn` spans two damage scans). Measured in scan
/// sequence, not wall time, so a late presentation cannot fake a fresh birth.
const REKEY_MAX_SEQ_GAP: u64 = 2;
/// Reading-order row window for rekeying a same-width occurrence: covers a
/// soft-wrap crossing or one inserted status row, while rejecting a log-tail
/// rotation whose new twin enters four rows away.
const REKEY_MAX_LINEAR_ROWS: u32 = 2;
/// One byte per cell in the bounded monotone alignment DP. Scores use two
/// rolling rows; only these decisions are retained for reconstruction.
const ALIGN_DECISION_CELLS: usize = (PERSIST_CAP + 1) * (MAX_OCCURRENCES + 1);
/// §3.6 ordinal mix constant: `ident = mix(seed ^ ordinal · THIS)`.
const ORDINAL_MIX: u64 = 0x9E37_79B9_7F4A_7C15;
/// Floor / ceiling for one generation of the [`ScanMemo`]; the live cap is the
/// viewport's row count clamped into this band (see `ScanMemo::fit`), so the
/// memo's footprint (≈ `2 · rows · cols` bytes) tracks the grid.
const SCAN_MEMO_MIN_GEN: usize = 32;
const SCAN_MEMO_MAX_GEN: usize = 4096;
/// §5.2 hard cap on peeking cats per frame: row-major deterministic truncation;
/// occurrences past the cap keep only their ink.
const MAX_CATS: usize = 8;
/// §5.6 entrance window (`CAT_RISE_MS ≈ 450`).
const CAT_RISE_MS: u64 = 450;
/// §5.6 kitten landing-bounce window after the rise (dest-rect offsets only).
const CAT_BOUNCE_MS: u64 = 150;
/// §5.7 readability floor: below these cell metrics there is no cat, only the
/// word's ink (a cat that cannot render eyes is worse than none).
const CAT_MIN_CELL_H: u16 = 14;
const CAT_MIN_CELL_W: u16 = 7;

/// Cap on animal-roster heads per frame, a pool separate from `MAX_CATS` so
/// neither family can starve the other. Row-major truncation; an occurrence
/// past the cap draws no graphic but keeps its ink.
const MAX_ANIMALS: usize = 8;
/// v3 §1.2 descend window (easeInOutCubic).
const CAT_DESCEND_MS: u64 = 320;
/// v3 §1.2 optional pre-descend anticipation lift (~50% of genomes).
const CAT_ANTICIPATION_MS: u64 = 60;
/// Peek dwell cap (v3 §1.2): the longest peek, `450 + 3750 + 380 = 4580 ms`,
/// ends before a driven capture's `t = 5 s` settled frame.
const CAT_DWELL_CAP_MS: u64 = 3750;
/// v3 §1.2 twin-desync dwell jitter: `dwell_base − mix(ident) % 300`.
const CAT_DWELL_JITTER_MS: u64 = 300;
/// v3 §1.2 magic/accessory dwell bonus (rare cats linger a beat longer).
const CAT_DWELL_MAGIC_BONUS_MS: u64 = 500;
/// Resize-settle window (v3 §1.1): births until the first rescan at stable
/// cols + this are born-settled (no entrance, no roll, static ink) and never
/// written to `done_marks`.
const RESIZE_SETTLE_MS: u64 = 500;
/// `done_marks` LRU capacity (v3 §1.1).
const DONE_MARKS_CAP: usize = 65_536;
/// Null link for the done-mark LRU's `u32` indices. The entry cap is far below
/// it, so every resident node has a representable index.
const DONE_MARK_NONE: u32 = u32::MAX;
/// v3 §1.2 graphics-decay rule: sparkle + nova ember residuals fade out
/// within this window after their animation window.
const RESIDUAL_FADE_MS: u64 = 2000;
/// Dwell derivation salts. The §3.4 cat genome has no dwell bits, so dwell
/// decodes from salted mixes of `gkey` (deterministic, dwell 2200..=3598).
const DWELL_SALT: u64 = 0xD3E1_15A1_C0FF_EE00;
const ANTIC_SALT: u64 = 0xA117_1C1F_7A11_57A6;
/// §6.4 flash-limiter rolling window: at most 2 ignitions per rolling second
/// window-wide (WCAG 2.3.1), tightening to ≤ 1 when regions overlap (center
/// distance < 2·R_max). Excess ignitions are delayed in row-major order, never
/// dropped. Modeled as `FlashLimiter` in aterm-spec.
const IGNITION_WINDOW: Duration = Duration::from_secs(1);
/// At most two already-fired reservations remain inside the disjoint-region
/// rolling window; pending ones are one per live persist episode, so the sum
/// is bounded even under a redraw flood.
const MAX_RECENT_IGNITIONS: usize = 2;
const MAX_IGNITION_RESERVATIONS: usize = PERSIST_CAP + MAX_RECENT_IGNITIONS;

/// v3 §6 `BurstKind::Glow` window: one soft rise-and-fall, then done.
const GLOW_BURST_MS: u64 = 1400;
/// Star halo riding the opening of every profanity rainbow. It fits inside the
/// rainbow drift, so it adds no deadline or idle wake of its own.
const RAINBOW_SPARKLE_MS: u64 = 1000;

/// App-cached sparkle state: the resolved [`DecoConfig`] plus the compiled
/// lexicon, rebuilt only on config reload / toggle, never per frame.
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

/// `[sparkle_words.profanity] style` (§10 / v3 §3.1): rainbow ink (default),
/// the classic nova, or the randomized sparkle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfanityStyle {
    /// `style = "rainbow"` (default): the §3.1 animated rainbow ink, with
    /// `supernova_chance`% of episodes escalating to the §3.2 supernova.
    Rainbow,
    /// `style = "nova"`: the §6 classic nova. No size floor — the ring band
    /// count clamps below 2r = 60 px (§6.3). Ignores `supernova_chance`.
    Nova,
    /// `style = "sparkle"`: the randomized sparkle (`supernova_chance` ignored).
    Sparkle,
}

/// A borrowed view of the terminal selection for the tick (§6.4): ignition
/// defers while the word is selected, and an active nova's quads attenuate
/// over selected cells (applied per quad host-side, so both backends stay
/// byte-identical). Selection rows are live rows; viewport rows convert
/// through `display_offset` as in the renderers.
#[derive(Clone, Copy)]
pub struct SelView<'a> {
    pub sel: &'a aterm_core::selection::TextSelection,
    pub display_offset: i32,
}

impl SelView<'_> {
    fn cell_selected(&self, row: u16, col: u16) -> bool {
        self.sel.contains(i32::from(row) - self.display_offset, col)
    }

    /// Any cell of the span `[c0, c1]` on `row` selected?
    fn span_selected(&self, row: u16, c0: u16, c1: u16) -> bool {
        (c0..=c1).any(|c| self.cell_selected(row, c))
    }
}

/// Per-frame effect geometry (§2.4 `EffectGeom`): the cell metrics and grid
/// extent the cat emitter sizes against. All-zero (the default) trips the §5.7
/// readability floor, so geometry-less callers draw no cat.
#[derive(Clone, Copy, Debug, Default)]
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

/// The host's per-frame view of the cursor companion, for the
/// one-cat-per-caret suppression ([`WordDecorations::tick`]). Hosts get
/// `body_px` from [`crate::kitty_pet::PetFrame::body_px`] or
/// [`WordDecorations::kitty_cursor_footprint`].
///
/// Two rects, not one union: the union would suppress legitimate "peeks
/// beside the companion" cats.
#[derive(Clone, Copy, Debug)]
pub struct CompanionOnGlass {
    /// The visible caret cell `(row, col)` in pre-splice grid coords. `None`
    /// only for a still-visible resident body whose caret is currently hidden.
    pub cell: Option<(u16, u16)>,
    /// The companion's live drawn sprite `(x0, x1, y0, y1)` in grid pixels,
    /// right/bottom exclusive. `None` only when the host could not resolve one
    /// this frame.
    pub body_px: Option<(i32, i32, i32, i32)>,
    /// Does the caret cell's own band belong to the companion as well? True
    /// for the caret-escorting flying head, false for the resident pet.
    pub guards_caret: bool,
}

impl CompanionOnGlass {
    /// The flying head with no resolved sprite rect: only the caret band
    /// stands in, under-covering the sprite. Prefer [`Self::at_head`].
    #[must_use]
    pub fn at_cell(cell: (u16, u16)) -> Self {
        Self {
            cell: Some(cell),
            body_px: None,
            guards_caret: true,
        }
    }

    /// The flying head with its live drawn rect, from
    /// [`WordDecorations::kitty_cursor_footprint`]. Guards the caret band too.
    #[must_use]
    pub fn at_head(cell: (u16, u16), head_px: (i32, i32, i32, i32)) -> Self {
        Self {
            cell: Some(cell),
            body_px: Some(head_px),
            guards_caret: true,
        }
    }

    /// A resident pet on glass. Its body is authoritative even while DECTCEM
    /// temporarily withholds the caret; callers must never guess a stale cell.
    #[must_use]
    pub fn at_body(cell: Option<(u16, u16)>, body_px: (i32, i32, i32, i32)) -> Self {
        Self {
            cell,
            body_px: Some(body_px),
            guards_caret: false,
        }
    }

    /// The rects an ambient cat must not stack on this frame.
    #[must_use]
    fn footprint(self, geom: EffectGeom) -> CompanionFootprint {
        CompanionFootprint {
            rects: [
                self.body_px,
                self.guards_caret
                    .then(|| {
                        let (row, col) = self.cell?;
                        let cw = i32::from(geom.cell_w);
                        let ch = i32::from(geom.cell_h);
                        let x0 = i32::from(col) * cw;
                        let y0 = i32::from(row) * ch;
                        // Two cells wide, the caret's own row band; not the
                        // head's extent (that is `body_px`), so a cat peeking
                        // up past the caret's rows still draws.
                        Some((x0, x0 + 2 * cw, y0, y0 + ch))
                    })
                    .flatten(),
            ],
        }
    }
}

/// The pixels the live cursor companion owns this frame — up to two rects; an
/// ambient cat yields if it overlaps either. See [`CompanionOnGlass`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompanionFootprint {
    rects: [Option<(i32, i32, i32, i32)>; 2],
}

impl CompanionFootprint {
    /// Is the cat rect `(x, w, top..bottom)` stacked on any companion rect?
    #[must_use]
    pub(crate) fn stacked_on(&self, x: i32, w: i32, top: i32, bottom: i32) -> bool {
        self.rects
            .iter()
            .flatten()
            .any(|rect| cat_is_stacked_on(*rect, x, w, top, bottom))
    }
}

/// One frame of the full-body pet, ready to draw. The brain
/// ([`crate::kitty_pet::PetBrain`]) resolves the animal; this adds what the
/// emitter needs — cell metrics, local colour context, coat/iris identity.
#[derive(Clone, Copy, Debug)]
pub struct PetCursorFrame {
    pub geom: EffectGeom,
    /// Quantized local foreground/background context (drives the contrast
    /// ink): the live body's. A ghost wears the palette its own body was
    /// drawn in (`PetDepartureSprite::colors`), this one only when it has
    /// none.
    pub colors: CatColorKey,
    /// `COAT_RAMP` / `EYE_RAMP` indices latched for this appearance
    /// ([`crate::kitty_pet::PetBrain::sync_look`]), so a mid-walk companion
    /// change cannot re-skin the pet on screen.
    pub coat: u8,
    pub iris: u8,
    pub pet: crate::kitty_pet::PetFrame,
    /// The light page under the pet: the terminal ground where it is no dark
    /// theme (`aterm_render::theme_is_dark`), `None` on a dark one. Its ♪ and
    /// ♥ then take the paper law's point-mark ink
    /// ([`crate::rainbow_kitty::ribbon::paper_mark_ink`]).
    pub page: Option<u32>,
}

/// One resolved cursor-companion emission (grouped so the render call does not
/// grow a positional argument list).
pub struct KittyCursorFrame {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    pub colors: CatColorKey,
    pub bob: f32,
    /// Whole rows the seat raises the head ([`KittyCursorLayout::lift`]).
    pub lift: f32,
    pub alpha: u8,
    /// The pose ([`crate::kitty_cursor::CatPose`]): squash/stretch and lean
    /// applied to the dest rect, plus the baked eye frame. The cat bakes at
    /// natural size (one cache key per eye frame) and is scaled only at draw.
    pub pose: crate::kitty_cursor::CatPose,
    /// Sing-along drive 0..=1 (`crate::kitty_sing`): scales note alpha for the
    /// wind-down crossfade. 0.0 with an empty `notes` ring does no note work.
    pub sing: f32,
    /// This frame's ♪/♫ sprites (cell-relative to the mouth anchor), capped at
    /// [`crate::kitty_sing::MAX_NOTES`]. Emitted just before the body, so notes
    /// are cursor-cat-only and load-shed with the sparkle branch.
    pub notes: [Option<crate::kitty_sing::NoteSprite>; crate::kitty_sing::MAX_NOTES],
}

/// One resolved typed-word dog cameo emission ([`crate::dog_cameo`]): breed,
/// coat, envelope alpha and entry-bounce offset — [`KittyCursorFrame`] minus
/// the persistent-companion parts.
#[derive(Clone, Copy, Debug)]
pub struct DogCameoFrame {
    pub geom: EffectGeom,
    /// The cursor cell `(row, col)` the dog visits.
    pub cursor: (u16, u16),
    /// The visit's rolled identity ([`crate::dog_cameo::DogCameo::look`]).
    pub look: crate::dog_cameo::DogLook,
    /// Quantized local terminal palette (shared with the cat bakes).
    pub colors: CatColorKey,
    /// Entry-bounce offset in cell fractions (`<= 0`: up); `0.0` under
    /// reduced motion.
    pub bob: f32,
    /// The lifecycle envelope's presence, `0..=255`.
    pub alpha: u8,
}

/// Geometry-only inputs for resolving the cursor companion before its local
/// palette is sampled.
#[derive(Clone, Copy, Debug)]
pub struct KittyCursorLayout {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    /// The hover bob, in cell heights: drawn as it is, never smoothed.
    pub bob: f32,
    /// Whole rows the companion router's seat raises the head off its rest (the
    /// one-row-up seat of `rainbow_kitty::companion::placement`). Unlike the bob
    /// it moves the rest position itself, which the settled follower glides
    /// ([`WordDecorations::kitty_cursor_at_placement`]).
    pub lift: f32,
}

/// The cursor companion's prospective pixel footprint, resolved by the same
/// sizing/placement code as emission so palette sampling matches the art.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatFootprint {
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
}

/// One cursor-kitty placement resolved once for palette sampling and sprite
/// emission. `sample` is always the live logical target, while the drawn body
/// may be leaving/arriving off glass, so sampling never walks off-screen cells.
#[derive(Clone, Copy, Debug)]
pub struct KittyCursorPlacement {
    pub sample: CatFootprint,
    leg: KittyCursorPlacementLeg,
    facing_left: bool,
    sampled_at: Option<Instant>,
}

impl KittyCursorPlacement {
    fn settled(sample: CatFootprint) -> Self {
        Self {
            sample,
            leg: KittyCursorPlacementLeg::Settled,
            facing_left: false,
            sampled_at: None,
        }
    }

    /// True for either visible half-beat and the off-stage seam between them.
    /// Emitters keep signed X instead of clamping onto the opposite edge.
    #[must_use]
    pub fn in_motion(self) -> bool {
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
    body_w: u16,
    body_h: u16,
    source: u64,
}

#[derive(Clone, Copy, Debug)]
struct KittyCursorActiveFold {
    started: Instant,
    direction: crate::kitty_cursor::CatFoldDirection,
    from: CatFootprint,
    from_sprite: Option<(i32, i32)>,
}

#[derive(Default)]
struct KittyCursorPlacementState {
    key: Option<KittyCursorPlacementKey>,
    last_target: Option<CatFootprint>,
    last_sprite: Option<(i32, i32)>,
    last_emitted_at: Option<Instant>,
    /// The settled follower's body origin, hover bob excluded (re-added at
    /// emit, so the 1.4 Hz hover is never attenuated). `None` between episodes,
    /// after a continuity reset and across fold legs; the next settled frame
    /// then seeds from `last_sprite` or snaps to the raw placement.
    settle: Option<(f32, f32)>,
    seen_fold: Option<(Instant, crate::kitty_cursor::CatFoldDirection)>,
    active: Option<KittyCursorActiveFold>,
}

/// The fully-resolved per-frame decoration settings (host config → engine).
#[derive(Clone, Debug)]
pub struct DecoConfig {
    /// Master + per-category gate already folded in: matching runs at all only
    /// when at least one category is on.
    pub profanity: bool,
    pub feline: bool,
    /// Canine family → the typed-word dog cameo (the input path). The screen
    /// scanner's ambient class default is inert, so this gates only the cameo.
    pub canine: bool,
    /// Emphasis words (user `extra_words` only; the builtin lexicon has none),
    /// ink only. Gates the class-default path; a custom override bypasses it.
    pub emphasis: bool,
    /// Animated glyph-ink shimmer over ink-bearing classes (emphasis + profanity
    /// + feline). `false` ⇒ zero [`InkCell`]s emitted.
    pub ink_enabled: bool,
    /// Ink tint vs the captured original fg, `0.0..=1.0` (§4.2 `strength`).
    pub ink_strength: f32,
    /// One specular sweep window, ms; clamped `350..=6000` by the resolver
    /// (floor 600 when `ink_loop`, the §6.4 flash margin).
    pub ink_sweep_ms: u32,
    /// Re-sweep while the word stays visible (keeps focused wakes live).
    pub ink_loop: bool,
    /// Forces the static path (no twinkle / no jitter / no pulse).
    pub reduced_motion: bool,
    /// Suppress all decorations while the alternate screen is active (default
    /// off). The host render gate reads this; the engine never inspects
    /// screen mode.
    pub suppress_in_alt_screen: bool,
    /// Decorate the bare 3-letter `cat` token (opt-in).
    pub allow_bare_cat: bool,
    /// Decorate a lone CJK cat ideograph (opt-in).
    pub cjk_single_char: bool,
    /// Feline graphic mode: authored cat or ink-only `paw`.
    pub feline_style: FelineStyle,
    /// `[sparkle_words.feline] magic` (§10): Fortune (1/512) / Nebula (1/1024)
    /// rare cats. `false` ⇒ every cat takes the ordinary build.
    pub feline_magic: bool,
    /// `[sparkle_words.profanity] style` (§10).
    pub profanity_style: ProfanityStyle,
    /// `[sparkle_words.profanity] magic` (§10): Quasar (1/512) / Singularity
    /// (1/1024) rare novas. `false` ⇒ every nova takes the ordinary build.
    pub profanity_magic: bool,
    /// `[sparkle_words.profanity] supernova_chance` (`0..=100`, 0 disables):
    /// the per-appearance supernova escalation chance, consulted only under
    /// [`ProfanityStyle::Rainbow`]. Safe to raise: `MAX_ACTIVE_SUPERNOVAE = 1`
    /// and the ignition limiter (`IGNITION_WINDOW` / `MAX_RECENT_IGNITIONS`)
    /// queue ignitions, so the flash budget holds.
    pub supernova_chance: u8,
    /// The custom-word spec table (`[[sparkle_words.custom]]`). Overrides win
    /// over class defaults and bypass the per-class enable gate.
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
    /// Empty in the common case.
    pub ignore: HashSet<String>,
}

impl DecoConfig {
    /// Scan options derived from the config (borrows the `ignore` set).
    ///
    /// Public so the typed-word detector ([`crate::kitty_summon`], fed by the
    /// host input path) scans with the same `allow_bare_cat` /
    /// `cjk_single_char` / `ignore` policy.
    pub fn scan_opts(&self) -> ScanOptions<'_> {
        ScanOptions {
            allow_bare_cat: self.allow_bare_cat,
            cjk_single_char: self.cjk_single_char,
            ignore: (!self.ignore.is_empty()).then_some(&self.ignore),
        }
    }
}

/// The per-class default spec from the live config knobs: Profanity = Rainbow
/// plus SuperNova@chance (or classic Nova / Sparkle), Feline = Cats plus
/// SelfGlow, Emphasis = TwoTone. TwoTone defaults carry the class base pair and
/// are genome-hue-nudged at emission (custom TwoTone colors apply raw, see
/// `Occurrence::custom`).
fn class_default_spec(class: Class, cfg: &DecoConfig) -> WordEffectSpec {
    match class {
        Class::Profanity => WordEffectSpec {
            graphic: None,
            ink: Some(InkSpec {
                colorway: match cfg.profanity_style {
                    ProfanityStyle::Rainbow => crate::spec::rainbow_ink().colorway,
                    // The nova/sparkle ink anchors are the class pair (the
                    // nova palette overrides them through `InkFx`).
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
                    chance_pct: cfg.supernova_chance.min(100),
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
        // Canine words both summon the typed dog cameo (input path) and peek
        // the roster's dog head on screen (Animal default, species `dog`).
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
        // Animal roster: the species-keyed peeking sprite (species rides the
        // Occurrence), with the feline self-glow as the word's caption.
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

/// §3.1 rainbow base hue, degrees — a salted decode of the frozen genome so
/// similar contexts start at similar (not identical) hues.
const RAINBOW_HUE_SALT: u64 = 0x5A1A_D0FF_BEAD_5EED;
pub(crate) fn rainbow_base_hue(gkey: u64) -> f32 {
    (mix(gkey ^ RAINBOW_HUE_SALT) % 360) as f32
}

/// The native launch defaults (`sparkle_deco_config` with no `[sparkle_words]`
/// table): all four families on, ink on. The master switch stays host-owned
/// (web embedders default it off).
impl Default for DecoConfig {
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

/// One identity episode in the §3.6 persist map: what must survive a brief
/// occlusion so a re-hit within grace is a continuation, not a rebirth.
#[derive(Clone, Copy)]
struct Episode {
    /// First appearance of this episode. Sparkle/ink windows key off it, so a
    /// grace-backed re-hit never restarts an animation.
    appeared: Instant,
    /// Refreshed on every rescan that sees the identity; the grace sweep at
    /// rescan end drops entries unseen for [`GRACE_TTL`].
    last_seen: Instant,
    /// Frozen at birth (persist miss) — the only time SimHash runs. Neighbor
    /// churn after birth cannot re-roll anything (§3.6 "frozen at birth").
    genome: Genome,
    /// Local palette frozen at birth with the genome, so a live cat's bake keys
    /// never change mid-animation.
    cat_colors: CatColorKey,
    /// One nova per episode (§3.6/§6.4): set when the nova window elapses (or
    /// the concurrency cap skips to Ember). Re-arms only on identity death
    /// (grace expiry / `reset()`); lives here so grace preserves it.
    nova_done: bool,
    /// The assigned ignition instant (§6.4): the Dip start, granted once by the
    /// flash limiter (possibly delayed past first sight) and then frozen, so
    /// the phase machine is a pure function of `now − nova_start`. `None` until
    /// eligible (unassigned, or deferred by a selection).
    nova_start: Option<Instant>,
    /// Kitty Log (§F4.2): this episode already queued its one sighting. Grace
    /// expiry and `reset()` drop the episode, so a later re-appearance counts
    /// again.
    logged: bool,
    /// The current position-bearing occurrence seed (v3 §1.1). A horizontal
    /// redraw updates it when the episode is rekeyed to a new column.
    seed: u64,
    /// Collision-free ID of the exact recognized lexicon surface (including
    /// its morphology variant): the horizontal-redraw group key, so a spent
    /// episode can move columns without relying on hash collisions.
    form_id: FormId,
    /// Most recently recognized context fingerprint. A changed-context rekey
    /// adopts it (the genome stays frozen), so expiry marks the layout that
    /// actually departed; also the strong context arm of continuity.
    ctx_fp: u64,
    /// Last viewport position, used by the two-dimensional redraw alignment.
    last_row: u16,
    last_col: u16,
    /// A rescan saw nonblank replacement content on this row while the form
    /// was absent — incremental retyping/overwrite rather than a blank
    /// clear/redraw. Feline words always re-arm on it; profanity also needs a
    /// token ending at the live caret, so retyped `fuck` re-arms but unrelated
    /// `fix` redraws do not.
    continuity_tainted: bool,
    /// The rescan sequence that last adopted this episode. Episodes not seen
    /// this rescan form the alignment's "old" set for their seed group.
    seen_seq: u64,
    /// v3 §1.1 per-axis done flags. `peek_done`: the graphic one-shot ran to
    /// completion (descend finished / paw faded out) — zero quads forever.
    peek_done: bool,
    /// The graphic one-shot started (the birth-latched phase clock began
    /// ticking) — the done-mark write condition on departure.
    peek_started: bool,
    /// Set at ignition grant (detonation is the point of no return); the
    /// sparkle-style burst sets it when its residual fade completes.
    burst_done: bool,
    burst_started: bool,
    /// v3 §1.1: ink sweep axis (started at first emitted ink; done when the
    /// sweep + fade window elapses / the feline glow self-terminates).
    sweep_done: bool,
    sweep_started: bool,
    /// Born from a `done_marks` hit (v3 §1.1): inert — contributes nothing to
    /// active_until, fingerprint folds, sightings or burst rolls; emits only
    /// the settled ink.
    born_done: bool,
    /// Born during the resize-settle window (v3 §1.1): the `born_done` inert
    /// emission, but never written to `done_marks`.
    born_settled: bool,
    /// v3 §1.2 arm freezing: Cat vs Paw (+ fallback cause), stored at first
    /// emission decision; every later tick dispatches from the stored arm.
    shown_as: Option<KittyShownAs>,
    /// The peek clock origin, latched at birth (never deferred on focus), so
    /// cats never replay on refocus. `None` draws nothing; [`Episode::fresh`]
    /// always latches it.
    phase_start: Option<Instant>,
    /// Clearance pause: the instant a stored-Cat arm lost [`cat_eligible`]
    /// mid-peek (text landed in the body footprint, a DECDWL flip). While set
    /// the peek clock is suspended; on the first eligible tick `phase_start`
    /// shifts forward by the paused span, so the cat resumes where it vanished
    /// instead of burning its one-shot while invisible.
    peek_pause: Option<Instant>,
    /// The armed peek's total length (rise + dwell + anticipation + descend),
    /// latched while the occurrence is visible. A scrolled-off mid-peek episode
    /// completes against it by wall clock, since the flag loop only walks
    /// visible occurrences.
    peek_total: u64,
    /// The burst-axis roll, decided once at birth
    /// (`mix(gkey ^ birth_seq ^ SUPERNOVA_SALT) % 100 < chance_pct`) for every
    /// [`BurstKind`] and stored so it transfers with row alignment. Always
    /// `false` for born-done/born-settled episodes.
    burst_roll: bool,
    /// Which detonation degree this episode drew, from the high half of the
    /// birth draw that set [`Self::burst_roll`] (`supernova::tier_of`). Stored
    /// so it transfers with alignment. Meaningful only with `burst_roll` and a
    /// `SuperNova` burst kind.
    burst_tier: supernova::SuperTier,
    /// The burst kind at birth (`None` = no burst axis). The §3.2 burst mutex
    /// reads it to tell a classic nova's window from a supernova's.
    burst_kind: Option<BurstKind>,
    /// Curse bonk one-shot latch: set only at the fresh birth of a Profanity
    /// episode with the typed witness (caret completion plus keystroke),
    /// cleared when the tick queues its [`CurseCue`] (a full cue vec leaves it
    /// armed). Inert births, adoptions and deferred ambiguous forms never set
    /// it.
    bonk_pending: bool,
    /// This episode's done mark is already stamped under its current
    /// `(ident, ctx_fp)` while resident ([`WordDecorations::stamp_spent_marks`]).
    /// A de-duplicator so the stamp pass does not touch the LRU every frame;
    /// cleared when alignment moves the episode to a new identity.
    ledger_stamped: bool,
}

impl Episode {
    /// A fresh (just-born) episode.
    fn fresh(now: Instant, genome: Genome, seed: u64, row: u16, seq: u64) -> Self {
        Episode {
            appeared: now,
            last_seen: now,
            genome,
            cat_colors: CatColorKey::default(),
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

    /// Whether any one-shot started — the done-mark write condition whenever
    /// the episode leaves the persist map. Born-settled episodes are never
    /// marked; born-done ones refresh their mark.
    fn one_shot_started(&self) -> bool {
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

    /// Born-done and born-settled episodes share the inert emission path.
    fn inert(&self) -> bool {
        self.born_done || self.born_settled
    }
}

/// Which edge fired a curse bonk. The two kinds carry different provenance
/// guarantees, so the host gates them separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurseCueKind {
    /// A Profanity episode born with the live caret one past the token and a
    /// committed keystroke within reach. Output, redraws and pastes never
    /// produce it.
    Typed,
    /// Supernova ignition grant (§3.2): an on-screen curse whose burst roll
    /// escalated is detonating. Fires for output content too (hence its own
    /// off-by-default host knob); rate-limited by `grant_ignition`, once per
    /// episode via `burst_done`.
    Detonated,
}

/// One recorded curse bonk trigger (like the trail's `SoundCue` and
/// [`KittySighting`]). The engine records when; the host drains after
/// [`WordDecorations::tick`] and owns all policy (`bonk`/`bonk_detonation`
/// knobs, focus, reduced motion, volume) before mapping it to
/// `SoundGesture::Words(WordGesture::Bonk)`.
#[derive(Clone, Copy, Debug)]
pub struct CurseCue {
    pub kind: CurseCueKind,
    /// The word's viewport position; `col` is the lead column — the host maps
    /// it to stereo pan exactly like the trail's arrival column.
    pub row: u16,
    pub col: u16,
}

/// Cap on this tick's recorded curse cues (cleared at tick start, so hostless
/// builds stay bounded). Truncated typed cues retry via their episode latch;
/// detonations are once per episode.
const MAX_CURSE_CUES: usize = 16;

/// One recorded ambient-peek landing, the positioned cue for the resident
/// pet's bat: a [`CurseCue`]-shaped record with the sprite's dest rect.
/// Recorded at the sighting edge (a body sprite landed), drained beside
/// [`WordDecorations::drain_kitty_sightings`] and forwarded to the pet brain,
/// which range-checks. Observation only; nothing rendered changes.
#[derive(Clone, Copy, Debug)]
pub struct PeekCue {
    /// The word's viewport cell (row, lead column) — the [`CurseCue`] shape.
    pub row: u16,
    pub col: u16,
    /// The landed head's dest rect `(x0, x1, y0, y1)` in grid px,
    /// right/bottom exclusive (the head sits rows away from its word).
    pub head_px: (i32, i32, i32, i32),
}

/// Cap on this tick's recorded peek cues (cleared at tick start). Landings are
/// once per episode, so a screen of felines stays under it.
const MAX_PEEK_CUES: usize = 8;

/// How long a committed edit keystroke stays a valid "a human typed here"
/// witness, in ms.
///
/// Caret position alone ([`PendingBirth::at_live_cursor`]) is no evidence of
/// typing: a shell or TUI parks the caret at the line it just drew, so a pure
/// redraw would re-arm a suppressed cat. Only a keystroke
/// ([`WordDecorations::note_typed_edit`]) tells typed from redrawn, and it is
/// the only re-arm (never a timer). The window bridges key commit and echo
/// arrival (sub-millisecond locally, a few hundred ms over a loaded ssh link).
pub(crate) const TYPED_EDIT_WITNESS_MS: u64 = 750;

/// How far from a token the recorded edit caret may sit and still count as an
/// edit of that token, in columns.
///
/// The witness carries where the key landed ([`WordDecorations::last_caret`])
/// so a key pressed elsewhere in the pane (a TUI search box) cannot re-arm a
/// word a repaint restores. The recorded caret is pre-echo, so typing the last
/// letter, backspacing a tail or a kill-word leave it a cell or two off the
/// token; two columns cover those. Rows get no slack: silence is the safe
/// direction for a once-per-word contract.
pub(crate) const TYPED_EDIT_REACH_COLS: u16 = 2;

/// The last committed edit keystroke — the causal witness the feline/profanity
/// re-arm demands on top of the caret's position.
#[derive(Clone, Copy, Debug)]
struct TypedEdit {
    /// When the key was committed (host input time, not frame time).
    at: Instant,
    /// Where the key landed: the caret last scanned in the pane the key went to
    /// ([`WordDecorations::caret_of_pane`], live or parked). `None` when no
    /// cursor-bearing rescan ran there or the pane is unknown; the cell test
    /// then abstains, and without a cursor `at_live_cursor` is never set.
    cell: Option<(u16, u16)>,
    /// The pane the key went to, or `None` from a host that binds no pane. A
    /// key typed in pane A must not license a re-arm in pane B.
    pane: Option<u64>,
    /// Whether a re-arm has already spent it: one keystroke re-arms at most one
    /// word, so repaints inside the window cannot ride the same key.
    spent: bool,
}

/// One deferred birth/adoption candidate, parked by `scan_row` for the
/// rescan-end row-anchored alignment pass (v3 §1.1). Deferred whenever the
/// `(seed, ordinal)` ident misses an episode on the same row (growth, shrink,
/// rotation, scroll); the stable-screen fast path never defers.
#[derive(Clone, Copy)]
struct PendingBirth {
    /// Index into the occurrence vec under construction.
    occ_idx: u16,
    /// The context fingerprint computed while the row's char stream was still
    /// in scope (the genome + done-mark key ingredient).
    ctx_fp: u64,
    /// The token ends exactly at the live input caret. With a tainted prior
    /// episode this is evidence of retyping rather than a passive redraw.
    live_caret_completion: bool,
    /// The live caret touches the token (inside it or the cell after): the
    /// "word just typed" witness for the feline re-arm. A spent episode absent
    /// for a full rescan may not transfer onto such a candidate, and a
    /// colliding done mark is deleted rather than honored when the keystroke
    /// witness agrees.
    at_live_cursor: bool,
}

/// Capacity policy for an episode entering the persistent identity map.
///
/// A currently observed episode wins a key collision with grace-only history.
/// Capacity uses one global LRU order over the incoming episode and all
/// residents, keeping the newest [`PERSIST_CAP`] entries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PersistAdmission {
    Observed,
    Grace,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct AlignScore {
    stationary: u16,
    matches: u16,
    seed_matches: u16,
    context_matches: u16,
    distance: u64,
}

impl AlignScore {
    fn with_edge(self, stationary: bool, tier: u8, distance: u32) -> Self {
        Self {
            stationary: self.stationary.saturating_add(u16::from(stationary)),
            matches: self.matches.saturating_add(1),
            seed_matches: self.seed_matches.saturating_add(u16::from(tier == 0)),
            context_matches: self.context_matches.saturating_add(u16::from(tier == 1)),
            distance: self.distance.saturating_add(u64::from(distance)),
        }
    }

    /// A physical survivor is the strongest evidence: pin it before maximizing
    /// transfer count. Distinguishes a log rotation (`[0,2] -> [2,4]`, row 2
    /// survives, row 4 is new) from a global insertion (`[0,2] -> [1,3]`, both
    /// transfer). Ties prefer stronger context and less motion.
    fn better_than(self, other: Self) -> bool {
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

/// Compatibility/cost for one old episode and one candidate in the same
/// [`FormId`] group. Same-seed vertical edges keep the bounded log-tail skip
/// policy. Changed-column edges accept exact context unless a feline episode
/// was tainted by retyping, or a tainted profanity episode ends at the live
/// caret. Recent same-width continuity is the weak redraw arm. Profanity
/// otherwise takes the conservative branch below, so composer churn cannot
/// make `fix` appear causal.
fn alignment_edge(
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
        && (occ.class == Class::Feline
            || (occ.class == Class::Profanity && candidate.live_caret_completion))
    {
        return None;
    }
    // A spent feline episode that missed a full rescan may not transfer onto
    // the candidate at the live caret: no tier can tell a redraw from a retype
    // there. Gap ≤ 1 (moved by scroll or reflow) still transfers.
    if occ.class == Class::Feline
        && ep.peek_done
        && candidate.at_live_cursor
        && seq.wrapping_sub(ep.seen_seq) >= 2
    {
        return None;
    }
    let width = u32::from(cols.max(1));
    let old_pos = u32::from(ep.last_row)
        .saturating_mul(width)
        .saturating_add(u32::from(ep.last_col));
    let new_pos = u32::from(occ.row)
        .saturating_mul(width)
        .saturating_add(u32::from(occ.start_col));
    let distance = old_pos.abs_diff(new_pos);
    let max_distance = width.saturating_mul(REKEY_MAX_LINEAR_ROWS);
    let recent = seq.wrapping_sub(ep.seen_seq) <= REKEY_MAX_SEQ_GAP;

    if recent && ep.seed == occ.seed {
        return (distance <= max_distance).then_some((0, distance));
    }
    if ep.ctx_fp == candidate.ctx_fp {
        return Some((1, distance));
    }
    // Profanity is conservative for the grace lifetime unless the taint +
    // live-caret guard above proved retyping: a spent surface that briefly
    // vanished under composer churn transfers even with changed context and
    // position, so old output cannot relight. A genuinely new token has no
    // spare old episode and is born normally.
    if occ.class == Class::Profanity {
        return Some((1, distance));
    }
    (same_width && recent && distance <= max_distance).then_some((2, distance))
}

/// One rate-limiter reservation, labeled with the requesting episode's persist
/// identity. Ownership matters only while `start` is in the future (cancelling
/// a vanished pending episode is safe); a slot at or before `now` stays as
/// rolling-window history regardless of its owner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct IgnitionReservation {
    /// The scan scope that requested it ([`WordDecorations::scan_scope`]). The
    /// rolling-window count spans every scope, but only the requesting scope
    /// may cancel a pending slot (other scopes' episodes are parked). An
    /// `Option` because 0 is a valid session id.
    pane: Option<u64>,
    owner: u64,
    start: Instant,
    center: (i32, i32),
}

/// One live (quad-emitting) nova this frame, resolved by the §6.4 prepass: the
/// read-only hand-off to the pure emitters and the blast coupling.
#[derive(Clone, Copy)]
struct NovaLive {
    /// Index into `WordDecorations::occ`.
    idx: u16,
    /// The granted Dip start (phase machine runs on `now − start`).
    start: Instant,
    /// Nova center in grid px (visual span midpoint × row-band center).
    center_px: (i32, i32),
    /// Genome ring radius in px.
    r_max: f32,
    feats: NovaFeatures,
    magic: Option<NovaMagic>,
}

/// One live (quad-emitting) supernova this frame (v3 §3.2), resolved by the
/// super prepass for the `supernova` emitters and the selection wash-split.
#[derive(Clone, Copy)]
struct SuperLive {
    /// Index into `WordDecorations::occ`.
    idx: u16,
    /// The granted ignition instant (phase machine runs on `now − start`).
    start: Instant,
    /// Detonation center in grid px.
    center_px: (i32, i32),
    /// Shockwave reach in px (`min(6 rows, grid extent)`).
    r_max: f32,
}

/// One matched word on the visible grid. `seed` encodes the word's column,
/// class and text (not its row), so the word keeps a stable identity across
/// vertical scrolls and unrelated same-row churn.
#[derive(Clone)]
struct Occurrence {
    row: u16,
    start_col: u16,
    end_col: u16,
    class: Class,
    /// Every language claiming the matched surface (from [`Match`]): the Kitty
    /// Log's attribution source (§F4.2). Not folded into `seed`, `ident` or the
    /// genome, and never emitted.
    langs: LangSet,
    /// Collision-free exact recognized surface (including morphology),
    /// independent of row/column.
    form_id: FormId,
    seed: u64,
    /// The §3.6 episode identity (`mix(seed ^ ordinal·ORDINAL_MIX)`), the
    /// persist-map key.
    ident: u64,
    appeared: Instant,
    /// The episode's frozen genome (§3.6), copied out of the persist map every
    /// rescan — tick never touches the map.
    genome: Genome,
    /// Start index of this word's lead cells in the resident
    /// `ink_base_fg`/`ink_cols` buffers (captured at rescan, §4.3).
    ink_base: usize,
    /// Number of lead cells captured for ink; `0` = no ink (class not
    /// ink-bearing, ink disabled, or past the `MAX_INK_CELLS` truncation).
    ink_cells: u16,
    /// The word's first lead cell's bg, the §4.3 legibility-guard reference
    /// (mixed-bg words are evaluated against that cell — documented coarse case).
    ink_bg: [u8; 3],
    /// Quantized matched-word + neighboring-text + local-background palette.
    /// Computed only on damage-epoch rescan; copied into the bounded bake key.
    cat_colors: CatColorKey,
    /// A peek direction was found for the body footprint: at least one side of
    /// the trigger row is clear enough for the head ([`cat_peek_plan`]). Cats
    /// draw [`FreeZ::UnderText`], so this only rejects rows where both sides
    /// are solid text.
    cat_text_clear: bool,
    /// The head direction: `true` peeks down from under the word (the top-row
    /// case, or when the two rows above are busier than the two below).
    cat_peek_down: bool,
    /// The row has a DEC double-width/height size (§5.7): no cat there; ink
    /// only.
    dec_line: bool,
    /// The episode's born-done/born-settled flags, copied at rescan (v3 §1.1):
    /// the inert emission gate (settled ink bytes only).
    inert: bool,
    /// The resolved effect spec (per-word override, else the class default,
    /// v3 §6): the tick dispatches on its axes, not on `class`.
    spec: WordEffectSpec,
    /// `spec` came from a `[[sparkle_words.custom]]` override: bypasses the
    /// class gate; custom TwoTone colors apply raw (no genome hue nudge).
    custom: bool,
    /// For a [`Class::Animal`] occurrence: the species head, resolved from the
    /// match's lexicon `species` tag ([`animal_glyph_from_key`]). `None` for
    /// other classes and for species without roster art (ink only).
    species: Option<AnimalGlyphId>,
}

/// Geometry-aware terminal surface used only by the rescan epilogue's
/// post-alignment text-clearance pass.
#[derive(Clone, Copy)]
struct CatSurface<'a> {
    cells: &'a [Vec<RenderCell>],
    geom: EffectGeom,
    feline_magic: bool,
}

/// One slot in the done-mark LRU's bounded intrusive list.
///
/// Key-to-slot lookup plus fixed-width neighbor indices keeps lookup, touch,
/// removal and eviction O(1), where a touch-sequence map would scan all
/// [`DONE_MARKS_CAP`] entries per insert into a full map. `nodes` and `free`
/// grow only to that cap.
#[derive(Clone, Copy, Debug)]
struct DoneMarkNode {
    key: u64,
    older: u32,
    newer: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DoneMarkMutation {
    evicted: Option<u64>,
    /// Intrusive-list node writes: the constant-work witness the
    /// `DoneMarkLruBound` binding in the tests checks.
    link_writes: u8,
}

/// Deterministic bounded LRU for completed one-shot identities.
struct DoneMarkLru {
    /// Keyed: a key is `ident ^ ctx_fp`, where `ctx_fp` is a SimHash of text
    /// the pane's program prints, mixed without a secret, so under Fx a program
    /// could precompute contexts that share one bucket and hold up to
    /// [`DONE_MARKS_CAP`] of them. Probed only at births and departures, so the
    /// keyed hash costs nothing measurable. Eviction order comes from `nodes`,
    /// never map iteration.
    index: KeyedHashMap<u64, u32>,
    nodes: Vec<DoneMarkNode>,
    free: Vec<u32>,
    oldest: u32,
    newest: u32,
}

impl Default for DoneMarkLru {
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

    fn clear(&mut self) {
        self.index.clear();
        self.nodes.clear();
        self.free.clear();
        self.oldest = DONE_MARK_NONE;
        self.newest = DONE_MARK_NONE;
    }

    /// Detach `idx` from the resident order. Returns the number of neighbor
    /// link writes (bounded; head/tail writes are constant).
    fn detach(&mut self, idx: u32) -> u8 {
        let node = self.nodes[idx as usize];
        let mut writes = 0u8;
        if node.older == DONE_MARK_NONE {
            self.oldest = node.newer;
        } else {
            self.nodes[node.older as usize].newer = node.newer;
            writes += 1;
        }
        if node.newer == DONE_MARK_NONE {
            self.newest = node.older;
        } else {
            self.nodes[node.newer as usize].older = node.older;
            writes += 1;
        }
        self.nodes[idx as usize].older = DONE_MARK_NONE;
        self.nodes[idx as usize].newer = DONE_MARK_NONE;
        writes + 2
    }

    fn attach_newest(&mut self, idx: u32) -> u8 {
        let mut writes = 2u8;
        self.nodes[idx as usize].older = self.newest;
        self.nodes[idx as usize].newer = DONE_MARK_NONE;
        if self.newest == DONE_MARK_NONE {
            self.oldest = idx;
        } else {
            self.nodes[self.newest as usize].newer = idx;
            writes += 1;
        }
        self.newest = idx;
        writes
    }

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

    fn remove(&mut self, key: &u64) -> bool {
        let Some(idx) = self.index.remove(key) else {
            return false;
        };
        self.detach(idx);
        self.free.push(idx);
        true
    }

    fn insert(&mut self, key: u64) -> DoneMarkMutation {
        self.insert_with_cap(key, DONE_MARKS_CAP)
    }

    /// Insert/touch with an explicit cap so the Tier-1 test can exhaust the
    /// exact shipping transition at a small state-space bound.
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

/// A user-supplied kitty-cursor sprite: the decoded native RGBA plus a cache of
/// the last nearest-resample to the current cursor target size.
struct KittyCustom {
    source_fp: u64,
    w: u16,
    h: u16,
    /// Shared: one decoded config sprite is published to every window, so a
    /// clone must not copy up to 4 MiB per window while scheduling a redraw.
    rgba: Arc<[u8]>,
    /// `(target_w, target_h, resampled_rgba)` — recomputed when the target
    /// (cell-metric-derived) size changes.
    cache: Option<(u16, u16, Vec<u8>)>,
}

/// Resident paint for one sing-along note tile. `host_tile` caches by id but
/// needs the slice again after an atlas eviction; keeping the last paint per
/// note kind makes steady-state probes allocation-free.
struct NotePaintCache {
    host: u64,
    rgba: Vec<u8>,
}

impl NotePaintCache {
    /// Land `host`'s `w × h` tile in the shared atlas from the paint in `slot`,
    /// repainting (`paint`) only when the host id changed. `None` means the
    /// shared bake budget is spent: it lands next frame.
    fn tile(
        slot: &mut Option<Self>,
        baker: &mut CatBaker,
        host: u64,
        (w, h): (u16, u16),
        paint: impl FnOnce() -> aterm_scene::Tile,
    ) -> Option<CatTile> {
        let cache = match slot {
            Some(cache) if cache.host == host => cache,
            _ => slot.insert(Self {
                host,
                rgba: paint().pixels().to_vec(),
            }),
        };
        baker.host_tile(host, w, h, &cache.rgba)
    }
}

/// Immutable host-authored source for the kitty cursor companion.
///
/// `Disabled` is distinct from `BuiltIn` so an invalid custom asset fails
/// closed instead of showing different art. Custom pixels are shared with the
/// host's config catalog; only target-size resamples are private.
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

/// Exact memo of `visible row text -> lexicon matches`, the rescan's dominant
/// cost.
///
/// Any PTY byte advances the damage epoch, so under output every frame
/// re-tokenises the viewport on the main thread, inside the present that
/// carries the keystroke echo. The scanner is a pure function of `(row text,
/// ScanOptions, Lexicon)`, so an unchanged row costs one hash lookup; everything
/// downstream of the match list still runs on every row.
///
/// Keyed by row text, not row index, so scrolled lines hit. Capacity is two
/// generations of `fit`: a full hot generation is demoted to cold wholesale
/// (dropping the previous cold). Retired buffers are kept in `spare` and
/// refilled, so an all-miss frame does not allocate per row.
///
/// Both generations are [`KeyedHashMap`]s because the keys are text a pane's
/// program prints: under unkeyed Fx, precomputed colliding rows would make a
/// rescan cost grow with the square of the viewport's rows.
#[derive(Default)]
struct ScanMemo {
    hot: KeyedHashMap<String, Vec<Match>>,
    cold: KeyedHashMap<String, Vec<Match>>,
    /// Emptied entry buffers from the generation `install` discarded, waiting
    /// to be refilled. Bounded by `gen_cap` (footprint ≈ 3 generations), making
    /// the all-miss steady state allocation-free.
    spare: Vec<(String, Vec<Match>)>,
    /// Live per-generation entry cap (from the viewport row count).
    gen_cap: usize,
    /// [`scan_inputs_fp`] of the gates and lexicon the entries were scanned
    /// under; a change retires them.
    inputs_fp: u64,
    /// Test-only: force every probe to miss so the equivalence battery can
    /// drive a second engine down the always-re-lex path.
    #[cfg(test)]
    bypass: bool,
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    misses: u64,
    /// Test-only: entries built from a fresh allocation because `spare` was
    /// empty. The all-miss allocation test asserts it stops growing.
    #[cfg(test)]
    fresh_buffers: u64,
    /// Test-only: take the unpooled miss path (allocate per miss, free the
    /// retired generation), so the cost probe can interleave both engines.
    #[cfg(test)]
    no_recycle: bool,
}

impl ScanMemo {
    fn clear(&mut self) {
        // Retirement (fingerprint change / `hard_reset`) drops the entries but
        // keeps their buffers: the whole viewport is re-scanned next frame.
        let keep = self.pool_target();
        Self::recycle(&mut self.spare, &mut self.hot, keep);
        Self::recycle(&mut self.spare, &mut self.cold, keep);
    }

    /// How many retired buffers the pool holds: one generation, exactly what
    /// the incoming generation asks `remember` for before the next rotation.
    fn pool_target(&self) -> usize {
        #[cfg(test)]
        if self.no_recycle {
            return 0;
        }
        self.gen_cap.max(1)
    }

    /// Move `gone`'s entry buffers into `spare` (up to `keep`), freeing the
    /// rest, and leave `gone` empty but allocated.
    fn recycle(
        spare: &mut Vec<(String, Vec<Match>)>,
        gone: &mut KeyedHashMap<String, Vec<Match>>,
        keep: usize,
    ) {
        let room = keep.saturating_sub(spare.len());
        // `take(room)` stops filling the pool at the cap; dropping the `Drain`
        // still removes the remaining entries.
        spare.extend(gone.drain().take(room));
        debug_assert!(gone.is_empty(), "the drain must empty the generation");
    }

    /// Per-rescan prologue: retire everything if the scan gates or the lexicon
    /// moved, and size the generations to the viewport.
    fn fit(&mut self, rows: usize, inputs_fp: u64) {
        if self.inputs_fp != inputs_fp {
            self.clear();
            self.inputs_fp = inputs_fp;
        }
        // Two screenfuls per generation: at one screenful a single new line
        // would rotate the generation every frame and force promotions.
        self.gen_cap = rows
            .saturating_mul(2)
            .clamp(SCAN_MEMO_MIN_GEN, SCAN_MEMO_MAX_GEN);
        // The spare pool tracks the viewport too, so a shrunken window or split
        // pane releases the big grid's buffers. No-op when it already fits.
        self.spare.truncate(self.gen_cap);
    }

    /// Probe for `text`, promoting a cold-generation hit into hot. `true` means
    /// [`matches`](Self::matches) will serve this row without a lexicon pass.
    fn touch(&mut self, text: &str) -> bool {
        #[cfg(test)]
        if self.bypass {
            self.misses += 1;
            return false;
        }
        if self.hot.contains_key(text) {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            return true;
        }
        let Some((key, value)) = self.cold.remove_entry(text) else {
            #[cfg(test)]
            {
                self.misses += 1;
            }
            return false;
        };
        #[cfg(test)]
        {
            self.hits += 1;
        }
        self.install(key, value);
        true
    }

    /// The memoized match list for a row [`touch`](Self::touch) reported
    /// resident. A miss would drop the row's decorations, so it is a debug
    /// assertion; release falls back to no matches.
    fn matches(&self, text: &str) -> &[Match] {
        debug_assert!(self.hot.contains_key(text), "touch() proved residency");
        self.hot.get(text).map_or(&[][..], Vec::as_slice)
    }

    /// Record a freshly scanned row, refilling a retired entry's buffers rather
    /// than allocating per miss (see the `spare` field).
    fn remember(&mut self, text: &str, matches: &[Match]) {
        // `bypass` switches the memo off entirely, bookkeeping included (the
        // equivalence battery's control engine, the cost probe's `memo=false`).
        #[cfg(test)]
        if self.bypass {
            return;
        }
        #[cfg(test)]
        if self.no_recycle {
            // The unpooled path, counted the same way for the allocation test.
            self.fresh_buffers += 1;
            self.install(text.to_owned(), matches.to_vec());
            return;
        }
        let (mut key, mut value) = self.spare.pop().unwrap_or_else(|| {
            #[cfg(test)]
            {
                self.fresh_buffers += 1;
            }
            (String::new(), Vec::new())
        });
        key.clear();
        key.push_str(text);
        value.clear();
        value.extend_from_slice(matches);
        self.install(key, value);
    }

    fn install(&mut self, key: String, value: Vec<Match>) {
        if self.hot.len() >= self.gen_cap.max(1) {
            // Generation rotation, not eviction: the screenful that went cold
            // still answers next frame's scroll, and the older generation is
            // retired in one walk, its buffers going to `spare`.
            let keep = self.pool_target();
            Self::recycle(&mut self.spare, &mut self.cold, keep);
            std::mem::swap(&mut self.hot, &mut self.cold);
        }
        self.hot.insert(key, value);
    }
}

/// Fold every scan-gate input that can change what
/// [`Lexicon::scan_into_with_scratch`] returns for a fixed row text into one
/// key. Computed once per rescan.
fn scan_opts_fp(opts: &ScanOptions<'_>) -> u64 {
    let mut fp = fnv1a_64(&[
        u8::from(opts.allow_bare_cat),
        u8::from(opts.cjk_single_char),
    ]);
    if let Some(ignore) = opts.ignore {
        // XOR-folded per-word hashes: `HashSet` iteration order is not stable,
        // so an order-sensitive fold would retire the memo spuriously. Length
        // is mixed in so a word that XORs to zero still moves the key.
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

/// The memo's full retirement key: the scan gates ([`scan_opts_fp`]) plus the
/// identity of the lexicon the entries were scanned against.
///
/// The scanner reads `(text, opts, lexicon)`, so a swapped lexicon (changed
/// `languages`, edited `[[entry]]` override, unloaded Toy Pack) must retire the
/// memo even without a `hard_reset`; `Match::form_id` is unique only within its
/// own instance. The key must be O(1) (keystroke path; aterm-lexicon exposes no
/// build digest):
///
/// * the instance address: production publishes a new `Arc` on rebuild, so the
///   address is stable for a live memo and distinct between live lexicons;
/// * `spaced_form_count()`, separating a rebuild that reuses the freed address.
///   Two lexicons at one address with equal counts would still collide.
fn scan_inputs_fp(lexicon: &Lexicon, opts: &ScanOptions<'_>) -> u64 {
    // `fnv_mix_u64` is the whole-scalar FNV-1a step, over the same prime the
    // gate fold above mixes bytes with.
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
    /// The latest instant any profanity occurrence is still animating; `None`
    /// when nothing is. Lets the scheduler poll `is_active` without the config.
    active_until: Option<Instant>,
    /// Resident scratch reused across rescans (rescan only runs on a damage epoch
    /// change, but reusing these avoids a per-row/per-line heap allocation).
    scan_cells: Vec<RenderCell>,
    scan_chars: Vec<char>,
    scan_matches: Vec<Match>,
    /// The rescan's tokenise+lexicon-lookup memo — see [`ScanMemo`]. Holds the
    /// only per-row work that is a pure function of the row's text.
    scan_memo: ScanMemo,
    /// Lexicon token/fold/CJK workspace, resident so typing-damage rescans
    /// allocate nothing after the scanner reaches its high-water capacities.
    scan_scratch: ScanScratch,
    /// Per-line reconstructed text + its char→column map, reused across rows.
    scan_text: String,
    scan_colmap: Vec<u16>,
    /// Whether each scanned viewport row had nonblank text. Consulted after
    /// alignment to taint unseen episodes without an O(rows × episodes) walk.
    scan_row_occupied: Vec<bool>,
    /// The pet's ink probe: each scanned viewport row's inked columns as a
    /// half-open `[first, end)` span, `(0, 0)` for a blank row, refreshed by
    /// every rescan from the lexicon's row walk. Hosts hand it to
    /// [`crate::kitty_pet::PetBrain::sense_ink`] via [`Self::pet_ink`]. Rows a
    /// saturated rescan skipped read blank.
    pet_ink_spans: Vec<(u16, u16)>,
    /// §3.6 identity persistence: `ident → Episode`, cap [`PERSIST_CAP`],
    /// oldest-`last_seen` eviction, [`GRACE_TTL`] sweep at rescan end.
    ///
    /// Fx, not keyed, like `seed_ordinals`. A pane's program steers these keys
    /// (an ident mixes `(column, class, lexicon form, ordinal)` without a
    /// secret), but only within a domain aterm sets: the grid's columns, the
    /// lexicon's forms and ordinals. Over every builtin form at every column, the
    /// worst shared bucket holds 13 idents at 80 columns, 23 at 200, 36 at 500,
    /// and [`PERSIST_CAP`] caps any chain. `tick` probes this map several times
    /// per occurrence per frame, and keying it measured 35-50% slower ticks. Keys
    /// from free text have no such bound and are keyed: see [`ScanMemo`] and
    /// [`DoneMarkLru::index`].
    ///
    /// Both iteration sites are order-independent (`align_pending` sorts;
    /// `rescan_end` mutates entries independently).
    persist: FxHashMap<u64, Episode>,
    /// Per-rescan row-major count of same-seed occurrences — the §3.6 ordinal
    /// source. Resident so the rescan allocates nothing after warmup.
    ///
    /// Fx for the reason on `persist`: a seed mixes `(column, class, lexicon
    /// form)`, at most 128 entries, worst shared bucket 98 seeds at 500 columns
    /// (a few microseconds per rescan).
    seed_ordinals: FxHashMap<u64, u64>,
    /// §3.2 SimHash vote accumulator (resident scratch; zero allocation).
    votes: VoteScratch,
    /// Resident raw-token / folded-token scratch for the miss-path context walk.
    ctx_tok: String,
    ctx_folded: String,
    /// Test-only grace override (`None` = [`GRACE_TTL`]). The SparkleIdentity
    /// conformance negative control sets `Some(ZERO)` to drive the `Buggy`
    /// trace.
    grace_override: Option<Duration>,
    /// Per-lead-cell base fg captured at rescan (§4.3), the `base_fg` term of
    /// the §4.2 ink mix; indexed by `Occurrence::ink_base`.
    ink_base_fg: Vec<[u8; 3]>,
    /// Per-lead-cell viewport column, parallel to `ink_base_fg` (wide
    /// continuation halves are never captured; the lead cell's InkCell governs
    /// the whole glyph).
    ink_cols: Vec<u16>,
    /// §5.5 peeking-cat bake cache (LRU tiles + the published atlas). Cleared
    /// on `reset()` and on cell-metric change.
    cat_baker: CatBaker,
    /// Whether `tick` already ran the baker prologue for this host frame. The
    /// cursor companion shares that budget; it must not reset the two-bake cap.
    cat_baker_ready: bool,
    /// The animal roster's exact-size tile cache ([`crate::animal_baker`]),
    /// handed to the shared atlas through `host_tile` like the pet's.
    animal_baker: AnimalBaker,
    /// The pet roster's exact-size tile cache ([`crate::pet_baker`]), reaching
    /// the screen through the shared atlas's `host_tile`: one slot per resident
    /// pose, nothing when the pet is off.
    pet_baker: crate::pet_baker::PetBaker,
    /// The last pet tile that resolved: `(ax, ay, pose, coat, iris)`. A frame
    /// whose bake was deferred re-draws it ([`WordDecorations::pet_cursor`]),
    /// but only while it wears this frame's `(coat, iris)`, so a breed swap
    /// never re-emits the old coat beside its departing ghost. Cleared with the
    /// atlas.
    pet_last_tile: Option<(u16, u16, crate::pet_glyphs_gen::PetGlyphId, u8, u8)>,
    /// Each departing body's last-resolved tile `(id, ax, ay)`, keyed by the
    /// ghost's id (`PetDepartureSprite::id`), not its lane slot, so a ghost
    /// keeps its hold across slots and a reborn slot never wears another's.
    /// Twice the lane, so one frame's ghosts never evict each other.
    pet_depart_tiles: [Option<(u32, u16, u16)>; PET_DEPART_HOLDS],
    /// The dog roster's exact-size tile cache ([`crate::dog_baker`]) for the
    /// typed-word dog cameo, through the shared atlas's `host_tile`.
    dog_baker: crate::dog_baker::DogBaker,
    /// The last dog tile that resolved: `(ax, ay, look)`. Deferred-bake
    /// fallback like [`Self::pet_last_tile`], refused when its breed differs:
    /// consecutive summons are distinct breeds
    /// ([`crate::dog_cameo::DogCameo::on_summon`]) and the dest rect uses the
    /// current breed's aspect ([`crate::dog_baker::DogBaker::aspect`]).
    dog_last_tile: Option<(u16, u16, crate::dog_cameo::DogLook)>,
    /// The cell metric the held tiles above were resolved against.
    ///
    /// `CatBaker::begin_frame` wipes the shared atlas when the metric moves, and
    /// a hold is a raw `(ax, ay)` band that would then name whatever re-baked
    /// there. Tracking the metric makes
    /// [`Self::drop_holds_if_metric_moved`] idempotent from every baker entry.
    held_tile_metric: (u16, u16),
    /// A user-supplied cursor sprite (`cursor_nyan_sprite`) overriding the
    /// built-in cat: native RGBA plus the last nearest-resample.
    kitty_custom: Option<KittyCustom>,
    /// Invalid authored sprite assets fail closed rather than falling back to
    /// built-in art while the UI reports a custom source.
    kitty_disabled: bool,
    /// Bumped whenever the cursor sprite source changes (custom set/cleared), so
    /// the shared-atlas host key changes and the stale tile is re-baked.
    kitty_gen: u64,
    /// Pixel-placement continuity for the flying cursor kitty: the last sprite
    /// origin across the off-glass seam (the brain owns only fold time and
    /// direction).
    kitty_cursor_placement: KittyCursorPlacementState,
    /// The sing-along's ♪/♫ note paints, one per
    /// [`crate::kitty_sing::NoteKind`], repainted only when the host id changes.
    note_cache: [Option<NotePaintCache>; 2],
    /// The nuke cloud's paints, one per [`crate::nuke::NukePart`], as in
    /// [`Self::note_cache`]: each part bakes once and animates by dest-rect
    /// transform, spending no bake budget after warmup.
    nuke_cache: [Option<NotePaintCache>; 3],
    /// Session-scoped LRU set of finished one-shots (v3 §1.1), keyed
    /// `ident ^ ctx_fp`, cap [`DONE_MARKS_CAP`], touch-on-hit. Survives
    /// `reset()` (which flushes pending marks into it); cleared only by
    /// [`WordDecorations::hard_reset`].
    done_marks: DoneMarkLru,
    /// Freeze instant while the host has suspended effects (perf_reduced latch
    /// / suppressed alt screen). `thaw` shifts stored timestamps forward by the
    /// freeze duration.
    frozen_at: Option<Instant>,
    /// The resize-settle deadline (v3 §1.1): births before it (or on a rescan
    /// that itself changed cols) are born-settled.
    settle_until: Option<Instant>,
    /// Whether the host cannot present this window (unfocused or
    /// deco-suspended), synced by [`Self::set_presentable`]. Inverted so the
    /// default is presentable, as a hostless/wasm build needs.
    away: bool,
    /// Set when presentability returns after an away span: the next rescan's
    /// births are born-settled. See [`Self::set_presentable`].
    spend_next_births: bool,
    /// v3 §1.1: rescan sequence counter — episodes stamp `seen_seq` on adopt,
    /// so alignment's "old" set is exactly the not-yet-adopted episodes.
    rescan_seq: u64,
    /// v3 §1.1: deferred birth/adoption candidates (resident scratch).
    pending: Vec<PendingBirth>,
    /// v3 §1.1 alignment scratch: the seed group's old episodes, pulled out of
    /// the persist map for rekeying (resident).
    align_old: Vec<(u64, Episode)>,
    /// One cat per caret: the occurrence ident the live cursor companion is
    /// answering. Released when the companion leaves glass; while it is up,
    /// re-resolved every tick to the word under the caret (or
    /// [`Self::caret_word`]) and kept over non-feline ground. A claim, not a
    /// caret test, because the companion outlives the caret's visit
    /// (`DISCOVERY_HOLD`); it follows the caret because the resident pet never
    /// leaves. Pane-scoped.
    companion_claim: Option<u64>,
    /// The occurrence the caret was last seen on, recorded by the rescan: the
    /// [`Self::companion_claim`] fallback, since the companion reaches glass a
    /// frame after the tick that summoned it. Overwritten only by a fresh hit;
    /// pane-scoped and reset-cleared like the claim.
    caret_word: Option<u64>,
    /// The caret cell the last cursor-bearing rescan observed: the place half
    /// of the edit witness ([`Self::note_typed_edit`] knows only when and which
    /// pane). It is the pre-echo caret, on (or one past) a word being typed.
    /// The cell-less `rescan` keeps the last value; pane-parked.
    last_caret: Option<(u16, u16)>,
    /// Alignment scratch (v3 §1.1): the DP's traceback decisions, one byte per
    /// (old, candidate) cell, [`ALIGN_DECISION_CELLS`] in all.
    ///
    /// Resident rather than a stack array, which would be a 66 KB memset on
    /// every deferring rescan (no cell is read before written). Lent to
    /// `align_pending` by `mem::take` and restored at the tail.
    align_decisions: Vec<u8>,
    /// Kitty Log sightings for this tick (§F4.2): resident, cap
    /// [`MAX_OCCURRENCES`], cleared at tick start so hostless builds stay
    /// bounded. Drained via [`WordDecorations::drain_kitty_sightings`].
    sightings: Vec<KittySighting>,
    /// Pre-loop scratch (§F4.2): idents whose episode has not logged a sighting,
    /// computed before the emission loop borrows `persist`.
    unlogged: Vec<u64>,
    /// This tick's curse bonk cues (resident, cap [`MAX_CURSE_CUES`], cleared at
    /// tick start). Drained via [`WordDecorations::drain_curse_cues`].
    curse_cues: Vec<CurseCue>,
    /// This tick's positioned ambient-peek landings for the pet's bat
    /// (resident, cap [`MAX_PEEK_CUES`], cleared at tick start). Drained via
    /// [`WordDecorations::drain_peek_cues`].
    peek_cues: Vec<PeekCue>,
    /// §6.4 flash-limiter reservations. Fired entries survive for the rolling
    /// window even if their episode departs; future entries survive only while
    /// their owner still references that slot. One pending slot per live owner
    /// plus at most two recent ignitions bounds it by
    /// [`MAX_IGNITION_RESERVATIONS`].
    ignitions: Vec<IgnitionReservation>,
    /// Per-tick scratch: this frame's live novas (≤ MAX_ACTIVE_NOVAS), read by
    /// the blast coupling (§6.5) and the burst arm.
    novas: Vec<NovaLive>,
    /// v3 §3.2 per-tick scratch: this frame's live supernova
    /// (≤ [`supernova::MAX_ACTIVE_SUPERNOVAE`] = 1; prepass-filled).
    supers: Vec<SuperLive>,
    /// §3.2 global burst mutex: the latest window end among granted (pending or
    /// live) supernovae; classic nova ignitions are deferred while inside it.
    super_until: Option<Instant>,
    /// §3.2 burst-mutex fast path: an upper bound on the end of any burst
    /// window this engine has granted, parked shards included. Once `now`
    /// reaches it, [`Self::super_prepass`] skips its busy scan (exact). It may
    /// over-hold (one wasted walk) but never under-hold, or two burst windows
    /// could overlap: it is folded at the two ignition grants and shifted by
    /// [`Self::thaw`], and nothing else can raise the true max.
    burst_hint: Option<Instant>,
    /// Session-scoped birth sequence, incremented on every persist miss: the
    /// supernova roll's decorrelation term (v3 §3.2). Survives `reset()` and
    /// `hard_reset()`.
    birth_seq: u64,
    /// §3.2b f-bomb combo: instants of recent typed f-bomb births (caret
    /// completions only). Window-wide like the flash limiter, since it is about
    /// the person at the keyboard. Bounded at [`supernova::COMBO_CAP`]; entries
    /// age out of [`supernova::COMBO_WINDOW_MS`] at the next charge, and
    /// `reset()`/`hard_reset()` clear it.
    combo_births: Vec<Instant>,
    /// The last committed edit keystroke ([`TypedEdit`]). Window-wide, not
    /// parked (one keyboard); its pane is stored in the value. No reset clears
    /// it: it self-expires after [`TYPED_EDIT_WITNESS_MS`].
    typed_edit: Option<TypedEdit>,
    /// The trick flash ([`crate::trick_flash`]): the word just typed to the pet
    /// (`sit`, the `good` of `good kitty`) flashing rainbow for about a second.
    /// One slot per engine, not parked (one keyboard); its pane is stored and
    /// compared to [`Self::scan_scope`]. Unlike `typed_edit`, a reset clears it
    /// (it holds grid cells and captured colours).
    trick_flash: TrickFlash,
    /// Per-tick scratch (§6.5): `(occurrence index, nova index)` pairs, each
    /// live nova's ≤ MAX_COUPLING_WORDS nearest ink-bearing occurrences.
    coupling: Vec<(u16, u8)>,
    /// Resident distance-sort scratch for the nearest-16 coupling selection.
    dist_scratch: Vec<(u64, u16)>,
    /// Episode/grid state of every pane except the bound one, whose state is in
    /// the live fields. See [`WordDecorations::bind_pane`].
    parked: FxHashMap<u64, ParkedPane>,
    /// Which pane the live fields hold; `None` for a host that never binds (the
    /// single-pane path), so binding is opt-in.
    bound: Option<u64>,
    /// The session the live fields describe while no pane is bound. See
    /// [`WordDecorations::set_scan_session`] and [`WordDecorations::scan_scope`].
    /// `None` only for a host that names no session; session tests then match
    /// anything.
    scan_session: Option<u64>,
    /// The bound pane's grid origin in window pixels. Ignition centers are
    /// shifted by it so the §6.4 flash limiter compares all panes in one space.
    pane_px: (i32, i32),
    /// Whether the host brackets frames with
    /// [`begin_host_frame`](WordDecorations::begin_host_frame). Latched on the
    /// first call: the bake budget is per frame, so multi-pane ticks must not
    /// multiply it; a non-bracketing host keeps one prologue per tick.
    host_brackets_frames: bool,
    /// The one-body law: has a cursor-companion body already reached this host
    /// frame's free stream? The host picks the owner
    /// ([`crate::companion::cursor_companion_duty`]); both companion emitters
    /// refuse a second body. Armed only for a bracketing host; gallery, preview
    /// and test hosts emit several companions into separate buffers.
    companion_body_drawn: bool,
}

/// One pane's episode + grid state while another pane is bound.
///
/// Panes in one window share one `WordDecorations`, hence one cat atlas, one
/// flash limiter and one supernova mutex (the §6.4 and §3.2 budgets are
/// window-wide). Grid-shaped and episode-scoped state parks here; binding
/// `mem::swap`s it in and out.
#[derive(Default)]
struct ParkedPane {
    occ: Vec<Occurrence>,
    last_epoch: u64,
    have_scanned: bool,
    cols: u16,
    frame: u64,
    active_until: Option<Instant>,
    scan_row_occupied: Vec<bool>,
    /// Grid-shaped like `scan_row_occupied`: one pane's ink map must never
    /// evict another pane's pet.
    pet_ink_spans: Vec<(u16, u16)>,
    /// Parked even though sharing would be correct: `ScanMemo::fit`'s
    /// `gen_cap = 2·rows` would thrash between panes of different heights.
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
    /// Parked because it is an occurrence ident: pane A's claim must not
    /// silence pane B's cat ([`WordDecorations::companion_claim`]).
    companion_claim: Option<u64>,
    /// Parked for the same reason as `companion_claim` — it is an occurrence
    /// ident (see [`WordDecorations::caret_word`]).
    caret_word: Option<u64>,
    /// Parked because it is a cell in one pane's grid — pane B's caret is not
    /// evidence about pane A's words (see [`WordDecorations::last_caret`]).
    last_caret: Option<(u16, u16)>,
    /// Derived: this shard's §3.2 burst-mutex summary, the latest end among its
    /// granted-but-unfinished burst windows (and the same for supernova
    /// windows). Recomputed by [`Self::swap`], the only path in or out of a
    /// parked shard, so it is never stale.
    ///
    /// Lets [`WordDecorations::super_prepass`] read a max instead of walking
    /// every parked map; exact, since `busy == (now < max(end))`.
    burst_end_max: Option<Instant>,
    super_end_max: Option<Instant>,
}

/// The §3.2b supernova's mutex-occupancy window: a Nuke holds it for its full
/// 3600 ms, and no tier goes below the classic 2400 ms (the emitter has no
/// short Flash arc).
fn super_window_of(tier: supernova::SuperTier) -> Duration {
    Duration::from_millis(supernova::total_ms(tier).max(supernova::SUPER_TOTAL_MS))
}

/// The instant a granted burst window ends: the §3.2 burst-mutex predicate,
/// shared by `super_prepass`'s live walk and [`ParkedPane`]'s summary. `None`
/// when the episode holds no window (finished, never granted, or a burst kind
/// without the mutex). The mutex is two-way: a live classic nova counts.
fn burst_mutex_end(ep: &Episode) -> Option<Instant> {
    if ep.nova_done {
        return None;
    }
    let start = ep.nova_start?;
    let win = match ep.burst_kind {
        Some(BurstKind::SuperNova) if ep.burst_roll => super_window_of(ep.burst_tier),
        Some(BurstKind::Nova) => {
            // Duration-only decode: this runs from full-map walks every tick
            // and needs one genome field.
            Duration::from_millis(u64::from(nova_duration_ms(ep.genome.gkey)))
        }
        _ => return None,
    };
    Some(start + win)
}

impl ParkedPane {
    /// A pane the engine has never driven. `away: true` (not `Default`) makes
    /// the host's following `set_presentable(now, true)` a real transition, so
    /// `spend_next_births` latches and a newly split pane full of feline words
    /// does not birth them all at once.
    fn fresh() -> Self {
        Self {
            away: true,
            ..Self::default()
        }
    }

    /// Exchange this parked state with the engine's live fields. Three words
    /// per `Vec`/`HashMap`; nothing is cloned and nothing reallocates.
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
        // `self.persist` now holds the map that just became parked (the swap is
        // symmetric). Re-derive its burst summary here, the one choke point;
        // edits to the live map are re-summarized on its next park.
        let (any_end, super_end) = self.burst_summary();
        self.burst_end_max = any_end;
        self.super_end_max = super_end;
    }

    /// This shard's §3.2 burst-mutex summary from `self.persist`: `(max end
    /// over any granted burst window, the same over supernova windows)`.
    /// `now`-free; see the field docs.
    fn burst_summary(&self) -> (Option<Instant>, Option<Instant>) {
        let (mut any, mut sup) = (None::<Instant>, None::<Instant>);
        for ep in self.persist.values() {
            let Some(end) = burst_mutex_end(ep) else {
                continue;
            };
            any = Some(any.map_or(end, |m: Instant| m.max(end)));
            if matches!(ep.burst_kind, Some(BurstKind::SuperNova)) {
                sup = Some(sup.map_or(end, |m: Instant| m.max(end)));
            }
        }
        (any, sup)
    }
}

impl WordDecorations {
    /// Record the one-shot guard before an episode leaves the persist map.
    fn mark_persist_departure(&mut self, ident: u64, ep: &Episode) {
        if ep.one_shot_started() {
            mark_done(&mut self.done_marks, ident, ep);
        }
    }

    /// Move an aligned episode and its limiter reservation in one rekey. A
    /// delayed nova keeps running from `ep.nova_start` after alignment; a
    /// reservation left under the old identity would be pruned by
    /// [`Self::prune_ignitions`], admitting an overlapping flash.
    fn rekey_persist_episode(&mut self, old_ident: u64, new_ident: u64, ep: Episode) -> bool {
        if old_ident != new_ident
            && let Some(start) = ep.nova_start
        {
            let mut remapped = 0usize;
            for reservation in &mut self.ignitions {
                if reservation.owner == old_ident && reservation.start == start {
                    reservation.owner = new_ident;
                    remapped += 1;
                }
            }
            debug_assert!(remapped <= 1, "one limiter slot per episode");
        }
        self.insert_persist_bounded(new_ident, ep, PersistAdmission::Observed)
    }

    /// The single bounded insertion funnel for the persist map.
    ///
    /// Alignment removes old episodes, admits visible ones, then offers
    /// unmatched history back for grace; funnelling every path here keeps the
    /// map at [`PERSIST_CAP`]. Key collisions favor an observed claimant;
    /// otherwise the oldest `(last_seen, ident)` in the resident-plus-incoming
    /// union departs with its done mark written.
    fn insert_persist_bounded(
        &mut self,
        ident: u64,
        ep: Episode,
        admission: PersistAdmission,
    ) -> bool {
        if self.persist.contains_key(&ident) {
            if admission == PersistAdmission::Grace {
                self.mark_persist_departure(ident, &ep);
                debug_assert!(self.persist.len() <= PERSIST_CAP);
                return false;
            }
            if let Some(displaced) = self.persist.remove(&ident) {
                self.mark_persist_departure(ident, &displaced);
            }
        }

        // Defensive repair if already over cap, then evict the oldest member
        // of the resident-plus-incoming union.
        while self.persist.len() > PERSIST_CAP {
            let Some(oldest) = self
                .persist
                .iter()
                .min_by_key(|&(key, resident)| (resident.last_seen, *key))
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(evicted) = self.persist.remove(&oldest) {
                self.mark_persist_departure(oldest, &evicted);
            }
        }
        if self.persist.len() == PERSIST_CAP {
            let oldest = self
                .persist
                .iter()
                .min_by_key(|&(key, resident)| (resident.last_seen, *key))
                .map(|(key, resident)| (*key, resident.last_seen));
            if oldest.is_none_or(|(oldest_ident, oldest_seen)| {
                (ep.last_seen, ident) <= (oldest_seen, oldest_ident)
            }) {
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
    /// Flushes every started episode's done mark into `done_marks` first, then
    /// drops per-occurrence state while keeping `done_marks`, so the next
    /// rescan re-enters finished words as born-done.
    #[cfg(test)]
    pub fn reset(&mut self) {
        self.with_each_pane(Self::reset_bound);
    }

    /// `reset` for the live fields only (the bound pane, or the whole window
    /// when nothing is bound); parked siblings are untouched, so retiring one
    /// pane leaves the other panes' cats alone.
    fn reset_bound(&mut self) {
        // Flush pass: marks written before persist.clear() (design §1.1).
        let marks = &mut self.done_marks;
        for (ident, ep) in &self.persist {
            if ep.one_shot_started() {
                mark_done(marks, *ident, ep);
            }
        }
        self.reset_transient_state();
    }

    /// Move the live fields into `key`'s parked slot (overwriting it), leaving
    /// a blank slate. Panes and declared sessions share one id space (the
    /// composed renderer's `bind_pane` key is `r.session`).
    fn park_live_state(&mut self, key: u64) {
        let mut slot = ParkedPane::default();
        slot.swap(self);
        self.parked.insert(key, slot);
    }

    /// Bind `key`'s state into the live fields, recording the pane's grid
    /// origin in window pixels (`px_origin`) for the shared flash limiter.
    ///
    /// Every visible pane drives this engine in turn, so each has its own
    /// damage epoch, column count, episode map, done marks and rescan sequence;
    /// otherwise panes would share spent one-shots, alternating widths would
    /// pin `born_settled`, and the `REKEY_MAX_SEQ_GAP` window would break.
    pub fn bind_pane(&mut self, key: u64, px_origin: (i32, i32)) {
        self.pane_px = px_origin;
        match self.bound {
            Some(cur) if cur == key => return,
            Some(cur) => self.park_live_state(cur),
            None => {
                // The live fields hold window-wide state (one pane covering the
                // whole grid) whose words just moved: the v3 §1.1 "pane-space
                // change → transient reset". Not `reset()`, which would also
                // wipe the correctly scoped parked panes. Nor a trick flash
                // found in a pane: it is that pane's (`retain_panes` already
                // dropped one whose pane left), so closing the bound pane
                // never kills a sibling's flash at the next bind.
                let flash = self
                    .trick_flash
                    .found_in_a_pane()
                    .then(|| std::mem::take(&mut self.trick_flash));
                self.reset_bound();
                if let Some(flash) = flash {
                    self.trick_flash = flash;
                }
            }
        }
        let mut slot = self.parked.remove(&key).unwrap_or_else(ParkedPane::fresh);
        slot.swap(self);
        self.bound = Some(key);
    }

    /// Declare which session the live fields describe on a host that binds no
    /// pane (the unsplit renderer and its capture twin). Without it `bound` is
    /// `None`, which session tests such as [`Self::typed_edit_witness`] read as
    /// a wildcard, so tab A's keystroke could re-arm a spent cat in tab B.
    /// Idempotent; single-grid hosts call it every frame before rescan/tick.
    ///
    /// - A change of session forces a rescan (two terminals' epoch counters
    ///   can coincide).
    /// - A declaration retires a disagreeing binding, parking the pane's state:
    ///   nothing unbinds when a window stops compositing, and a stale `bound`
    ///   would refuse the unsplit tab's own keystrokes.
    /// - Each tab parks its episodes under its session: idents fold no row or
    ///   session, so a shared map would let tab A's spent episode answer for
    ///   tab B. Keys match `bind_pane`'s, pruned by [`Self::retain_panes`].
    /// - Declaring never binds: `pane_px`, `retain_panes` and `caret_of_pane`
    ///   read `bound` as "a composed host is compositing this pane".
    /// - The first declaration adopts the live fields as that session's own;
    ///   a fresh slate would replay its cats.
    pub fn set_scan_session(&mut self, session: Option<u64>) {
        // Whether the live fields still hold the outgoing declaration's state.
        // Retiring a disagreeing binding parks the pane's state and leaves a
        // blank slate that must not be filed under `scan_session` (that key
        // may hold the newer pane copy of the same session).
        let mut live_is_declared = true;
        if let Some(cur) = self.bound
            && Some(cur) != session
        {
            self.park_live_state(cur);
            self.bound = None;
            // Unbound, the honest grid origin is the window's own — the same
            // reason `retain_panes` resets it when the live pane departs.
            self.pane_px = (0, 0);
            live_is_declared = false;
        }
        // The idempotent frame: same session, live fields already its own.
        if live_is_declared && self.scan_session == session {
            return;
        }
        if live_is_declared {
            match self.scan_session {
                Some(prev) => self.park_live_state(prev),
                // First declaration: adopt the live fields in place (above).
                None => {
                    self.scan_session = session;
                    self.have_scanned = false;
                    return;
                }
            }
        }
        self.scan_session = session;
        if let Some(next) = session {
            // `fresh` (away = true), as in `bind_pane`: a background tab
            // scanned nothing, so the next `set_presentable(_, true)` must be a
            // real transition or its whole feline backlog is born at once.
            let mut slot = self.parked.remove(&next).unwrap_or_else(ParkedPane::fresh);
            slot.swap(self);
        }
        self.have_scanned = false;
    }

    /// Which session the live fields describe: the bound pane if a composed
    /// host bound one, else the session an unsplit host declared
    /// ([`Self::set_scan_session`]). `None` only when unknown; session tests
    /// then match everything.
    fn scan_scope(&self) -> Option<u64> {
        self.bound.or(self.scan_session)
    }

    /// Declare the absolute row of the top visible line (`RenderInput::base_y`,
    /// read under the same lock as the cells) for the next rescan in the current
    /// scan scope, beside [`Self::set_scan_session`] / [`Self::bind_pane`].
    ///
    /// The trick flash uses it to find `sit⏎` after the echo scrolled: it tests
    /// row `caret.row − (base_y_now − base_y_then)`. Without it every delta
    /// reads as zero and the flash falls back to a bounded upward search. Spent
    /// by the next rescan in its scope.
    pub fn set_scan_base_y(&mut self, base_y: Option<i64>) {
        let scope = self.scan_scope();
        self.trick_flash.declare_base_y(scope, base_y);
    }

    /// Open a host frame, before any pane's [`tick`](Self::tick).
    ///
    /// The `MAX_BAKES_PER_FRAME` budget and the baker's LRU clock are per
    /// presented frame: a window compositing N panes bakes at most two tiles in
    /// total. Latches `host_brackets_frames`; a host that never calls this gets
    /// one prologue per tick.
    pub fn begin_host_frame(&mut self) {
        self.host_brackets_frames = true;
        self.cat_baker_ready = false;
        // A new frame: the one-body law's door reopens for exactly one
        // companion body ([`Self::companion_body_drawn`]).
        self.companion_body_drawn = false;
    }

    /// Drop parked state for panes this window no longer shows, and unbind the
    /// live state if its pane is gone. Hosts call this once per frame with the
    /// layout they are about to compose.
    ///
    /// `keep` is also asked about tabs parked by [`Self::set_scan_session`]. A
    /// host naming only the panes it draws evicts other tabs' episodes, which
    /// costs a replay of their cats (never a wrong episode); name every open
    /// session to avoid that.
    pub fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        // The trick flash is not parked, so it prunes itself (a departed pane's
        // base-y memo, a flash naming a gone pane).
        self.trick_flash.retain_panes(&keep);
        self.parked.retain(|k, _| keep(*k));
        if let Some(cur) = self.bound
            && !keep(cur)
        {
            self.bound = None;
            // With no pane bound the grid origin is the window's own.
            self.pane_px = (0, 0);
            // Reset only the departed pane's state (in the live fields); parked
            // siblings keep their episodes. The window-scoped trick flash
            // already pruned itself above and rides through this reset.
            let flash = std::mem::take(&mut self.trick_flash);
            self.reset_bound();
            self.trick_flash = flash;
        }
    }

    /// Run `f` with every pane's state bound in turn (the current state first,
    /// then each parked pane), restoring the original binding afterwards.
    fn with_each_pane(&mut self, mut f: impl FnMut(&mut Self)) {
        f(self);
        if self.parked.is_empty() {
            return;
        }
        let keys: Vec<u64> = self.parked.keys().copied().collect();
        for key in keys {
            let Some(mut slot) = self.parked.remove(&key) else {
                continue;
            };
            slot.swap(self);
            f(self);
            slot.swap(self);
            self.parked.insert(key, slot);
        }
    }

    /// Hard reset (v3 §1.1: master toggle, config reload / lexicon rebuild, web
    /// knob setters): clears episodes and `done_marks`, so everything replays.
    pub fn hard_reset(&mut self) {
        self.with_each_pane(|wd| {
            wd.done_marks.clear();
            wd.reset_transient_state();
        });
    }

    /// Reset only Sparkle Words state, keeping the cursor companion's placement
    /// and sprite caches. Used when the `[sparkle_words]` feed changes: word
    /// episodes retire immediately, but companions neither jump nor rebake.
    pub fn hard_reset_words(&mut self) {
        self.with_each_pane(|wd| {
            wd.done_marks.clear();
            wd.reset_word_transient_state();
        });
    }

    /// The shared body of `reset` / [`hard_reset`](Self::hard_reset):
    /// everything except the `done_marks` policy.
    fn reset_transient_state(&mut self) {
        self.reset_word_transient_state();
        self.kitty_cursor_placement = KittyCursorPlacementState::default();
        // §5.5 invalidation: a full reset drops every shared bake cache.
        self.cat_baker.clear();
        self.pet_baker.clear();
        self.animal_baker.clear();
        self.pet_last_tile = None;
        self.pet_depart_tiles = [None; PET_DEPART_HOLDS];
        self.dog_baker.clear();
        self.dog_last_tile = None;
    }

    /// Drop every held tile when the shared atlas is about to be cleared for a
    /// new cell metric (font-size step, DPI or monitor change), the other door
    /// out of the atlas beside [`Self::reset_transient_state`]. A surviving hold
    /// would name a band re-baked with different art.
    ///
    /// Idempotent: compares the metric and returns, so every companion entry
    /// can call it.
    fn drop_holds_if_metric_moved(&mut self, cell_w: u16, cell_h: u16) {
        if (cell_w, cell_h) == self.held_tile_metric {
            return;
        }
        self.held_tile_metric = (cell_w, cell_h);
        self.pet_last_tile = None;
        self.pet_depart_tiles = [None; PET_DEPART_HOLDS];
        self.dog_last_tile = None;
    }

    /// The per-frame baker prologue every atlas-baking entry runs first: LRU
    /// clock + bake budget; a cell-metric change clears and version-bumps the
    /// atlas (§5.5). Guarded by `cat_baker_ready`, so N panes share one
    /// two-bake budget; unbracketed hosts clear the flag at tick start.
    /// `animals` also begins the animal roster's cache (dog entries never
    /// bake one).
    fn begin_baker_frame(&mut self, geom: EffectGeom, animals: bool) {
        if self.cat_baker_ready {
            return;
        }
        self.drop_holds_if_metric_moved(geom.cell_w, geom.cell_h);
        self.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
        if animals {
            self.animal_baker.begin_frame(geom.cell_w, geom.cell_h);
        }
        self.cat_baker_ready = true;
    }

    /// Word-owned half of [`Self::reset_transient_state`]. Companion placement
    /// and the shared atlas/bakers stay out of it.
    fn reset_word_transient_state(&mut self) {
        self.occ.clear();
        self.have_scanned = false;
        self.last_epoch = 0;
        self.active_until = None;
        // The memo is valid only for the lexicon and gates that produced it.
        self.scan_memo.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        self.persist.clear();
        self.seed_ordinals.clear();
        self.pending.clear();
        self.align_old.clear();
        // The claim names an ident from the occurrence set that just died; a
        // later rescan can mint the same ident for a different word. The
        // caret-word memory dies with it.
        self.companion_claim = None;
        self.caret_word = None;
        self.frozen_at = None;
        self.settle_until = None;
        // §F4.2: pending sightings die with the state.
        self.sightings.clear();
        self.unlogged.clear();
        // Curse bonk cues die with the state too (their episode latches just
        // cleared with `persist` — no orphaned sound may outlive its word).
        self.curse_cues.clear();
        // Peek cues too: no orphaned bat may outlive its peek.
        self.peek_cues.clear();
        // Nova state dies with the occurrence set: pending/spent limiter slots
        // and the per-tick nova scratch all clear (byte-identical off).
        self.ignitions.clear();
        self.novas.clear();
        self.coupling.clear();
        // Supernova scratch and burst mutex clear with the nova state;
        // `birth_seq` survives so the roll stays decorrelated across resets.
        self.supers.clear();
        self.super_until = None;
        // A fresh start cools the f-bomb combo (§3.2b); surviving heat would
        // force escalated rolls across rebirths.
        self.combo_births.clear();
        // The trick flash holds cells and colours from the dead grid, and a
        // reload may have disabled tricks or ink. It is window-scoped, so the
        // per-pane fold runs this once per pane; `clear` is idempotent.
        self.trick_flash.clear();
    }

    /// Suspend the engine's clocks (perf_reduced latch, suppressed alt screen).
    /// Idempotent: the first call latches the freeze instant.
    pub fn freeze(&mut self, now: Instant) {
        if self.frozen_at.is_none() {
            self.frozen_at = Some(now);
        }
    }

    /// Resume after [`freeze`](Self::freeze): shifts every stored timestamp
    /// (`appeared`, `last_seen`, `nova_start`, `phase_start`, `peek_pause`,
    /// limiter slots, `active_until`, the settle deadline, the burst mutex and
    /// its hint) forward by the freeze duration, so animations resume where
    /// they paused and occluded episodes keep their remaining grace. No-op when
    /// not frozen.
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
            if let Some(s) = ep.nova_start {
                ep.nova_start = Some(s + d);
            }
            if let Some(s) = ep.phase_start {
                ep.phase_start = Some(s + d);
            }
            // Shifting the clearance pause with the phase clock keeps
            // `now − peek_pause` invariant across a freeze.
            if let Some(s) = ep.peek_pause {
                ep.peek_pause = Some(s + d);
            }
        }
        for occ in &mut self.occ {
            occ.appeared += d;
        }
        for reservation in &mut self.ignitions {
            reservation.start += d;
        }
        if let Some(a) = self.active_until {
            self.active_until = Some(a + d);
        }
        if let Some(s) = self.settle_until {
            self.settle_until = Some(s + d);
        }
        if let Some(s) = self.super_until {
            self.super_until = Some(s + d);
        }
        // The burst-mutex hint bounds `nova_start + window`, so it shifts with
        // the episodes' `nova_start`.
        if let Some(h) = self.burst_hint {
            self.burst_hint = Some(h + d);
        }
        // Combo heat is not thaw-shifted: `frozen_at` is pane-scoped while the
        // ring is window-wide, so shifting would push sibling panes' entries
        // into the future. Heat decays in engine-clock time. The trick flash is
        // likewise window-scoped and not shifted; a frozen pane neither finds
        // nor paints it.
    }

    /// §3.2b f-bomb combo level at `now`: prune entries older than
    /// [`supernova::COMBO_WINDOW_MS`], optionally charge (a typed f-bomb
    /// birth), and return the count for [`supernova::combo_chance`] /
    /// [`supernova::tier_for`]. Aging saturates because the introspection
    /// capture path may hand in a rewound `now`.
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

    /// Drain this tick's Kitty Log sightings (§F4.2), keeping capacity. Hosts
    /// drain at both tick sites (render and introspection); undrained growth
    /// is bounded by the tick-start clear.
    pub fn drain_kitty_sightings(&mut self) -> std::vec::Drain<'_, KittySighting> {
        self.sightings.drain(..)
    }

    /// Drain this tick's curse bonk cues, keeping capacity. Hosts drain right
    /// after [`tick`](Self::tick) (the vec clears at the next tick start); a
    /// disabled bonk knob drains and drops.
    pub fn drain_curse_cues(&mut self) -> std::vec::Drain<'_, CurseCue> {
        self.curse_cues.drain(..)
    }

    /// Drain this tick's positioned ambient-peek landings (the pet's bat).
    /// Drain right after [`tick`](Self::tick); a pet-less host drains and
    /// drops.
    pub fn drain_peek_cues(&mut self) -> std::vec::Drain<'_, PeekCue> {
        self.peek_cues.drain(..)
    }

    /// Sync whether the host can present this window (focused and not
    /// deco-suspended). Nothing is scanned while away, so on return
    /// `spend_next_births` latches: the next rescan's births are born-settled,
    /// as if their peek elapsed unwatched. `now` is unused (API compatibility).
    pub fn set_presentable(&mut self, _now: Instant, presentable: bool) {
        if self.away != presentable {
            return; // no transition (`away` is the inverse of `presentable`)
        }
        self.away = !presentable;
        if presentable {
            self.spend_next_births = true;
        }
    }

    /// The host's last [`Self::set_presentable`] verdict.
    #[must_use]
    pub fn presentable(&self) -> bool {
        !self.away
    }

    /// The versioned atlas paired with this frame's `free_sprites` output
    /// (exact-size tiles, art at tile row 0). Hosts copy it into
    /// `RenderInput::free_atlas` only when free sprites were emitted.
    pub fn free_atlas(&mut self) -> Option<std::sync::Arc<aterm_render::SceneAtlas>> {
        self.cat_baker.atlas()
    }

    /// The flying cursor kitty's dest rect for `layout`, from the emitter's own
    /// sizing and placement math. It flies 3/4 cell
    /// ([`Self::KITTY_LEAD_NUM`]/[`Self::KITTY_LEAD_DEN`]) past the cursor
    /// cell's right edge, centred on its row; near the right margin it clamps
    /// to the grid edge, rising up to half a cell rather than covering glyphs.
    /// `None` when disabled, geometry-less, or the grid is narrower than the cat.
    #[must_use]
    pub fn kitty_cursor_footprint(&self, layout: KittyCursorLayout) -> Option<CatFootprint> {
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
        let ch = usize::from(geom.cell_h);
        let slot_w = 4 * ch;
        let slot_h = 2 * ch;
        let (w, h) = if let Some(cust) = self.kitty_custom.as_ref() {
            let (nw, nh) = (usize::from(cust.w).max(1), usize::from(cust.h).max(1));
            let max_h = (ch * 9 / 5).clamp(1, slot_h);
            let s = (slot_w as f32 / nw as f32).min(max_h as f32 / nh as f32);
            (
                ((nw as f32 * s).round() as usize).clamp(1, slot_w) as u16,
                ((nh as f32 * s).round() as usize).clamp(1, slot_h) as u16,
            )
        } else {
            let look = look.normalized();
            let desired_h = Self::COMPANION_CELL_H * f32::from(geom.cell_h) * look.age.scale();
            authored_cat_size(look.variant, desired_h, geom.cell_h)
        };
        let cw = i32::from(geom.cell_w);
        let ch_i = i32::from(geom.cell_h);
        // Fly just ahead of the cursor (the 3/4-cell lead). Near the right
        // margin the cat clamps to the grid edge instead of vanishing, gliding
        // to a stop until the wrap gives it room.
        let grid_w = i32::from(geom.cols).saturating_mul(cw);
        let cursor_right = i32::from(ccol).saturating_add(1).saturating_mul(cw);
        let lead = Self::kitty_cursor_lead_px(geom.cell_w);
        let x = cursor_right
            .saturating_add(lead)
            .min(grid_w.saturating_sub(i32::from(w)));
        if x < 0 {
            return None; // the grid is narrower than the cat itself
        }
        // Boundary rise: when the edge clamp pushes the sprite back over the
        // cursor cell, the cat rises in proportion to the intrusion, capped at
        // `ch/2`, and backed off at the top edge so a top-row flight stays on
        // the grid.
        let rest_top = i32::from(crow) * ch_i + ch_i / 2 - i32::from(h) / 2;
        let intrusion = cursor_right - x;
        let rise = if intrusion > 0 {
            intrusion.min(ch_i / 2).min(rest_top.max(0))
        } else {
            0
        };
        let y = rest_top - rise - (lift * ch_i as f32).round() as i32
            + (bob * ch_i as f32).round() as i32;
        Some(CatFootprint { x, y, w, h })
    }

    /// Resolve the cursor kitty's logical target and line-fold placement once
    /// for this frame. `sample` stays at the live caret for palette selection;
    /// only the draw leg leaves and arrives off glass.
    #[must_use]
    pub fn resolve_kitty_cursor_placement(
        &mut self,
        layout: KittyCursorLayout,
        motion: crate::kitty_cursor::CursorCatPlacementFrame,
    ) -> Option<KittyCursorPlacement> {
        let Some(target) = self.kitty_cursor_footprint(layout) else {
            // A missing target breaks the coordinate space: retire every cached
            // origin so the next appearance starts at the live caret.
            self.kitty_cursor_placement = KittyCursorPlacementState::default();
            return None;
        };
        let key = KittyCursorPlacementKey {
            scope: self.scan_scope(),
            cell_w: layout.geom.cell_w,
            cell_h: layout.geom.cell_h,
            rows: layout.geom.rows,
            cols: layout.geom.cols,
            body_w: target.w,
            body_h: target.h,
            source: self.kitty_gen,
        };
        let stale = self
            .kitty_cursor_placement
            .last_emitted_at
            .is_none_or(|at| {
                motion.sampled_at.saturating_duration_since(at)
                    >= crate::kitty_cursor::CURSOR_FOLD_DURATION
            });
        if self.kitty_cursor_placement.key != Some(key) || stale {
            self.kitty_cursor_placement.key = Some(key);
            self.kitty_cursor_placement.last_target = Some(target);
            self.kitty_cursor_placement.last_sprite = None;
            // The settled follower dies with its continuity: snap to the live
            // caret rather than swoop across the discontinuity.
            self.kitty_cursor_placement.settle = None;
            self.kitty_cursor_placement.active = None;
            // A fold under way belongs to the retired coordinate space; mark it
            // seen so it cannot restart from the new target.
            self.kitty_cursor_placement.seen_fold =
                motion.fold.map(|fold| (fold.started, fold.direction));
            return Some(KittyCursorPlacement {
                sample: target,
                leg: KittyCursorPlacementLeg::Settled,
                facing_left: motion.facing_left,
                sampled_at: Some(motion.sampled_at),
            });
        }

        let Some(fold) = motion.fold else {
            self.kitty_cursor_placement.active = None;
            self.kitty_cursor_placement.last_target = Some(target);
            return Some(KittyCursorPlacement {
                sample: target,
                leg: KittyCursorPlacementLeg::Settled,
                facing_left: motion.facing_left,
                sampled_at: Some(motion.sampled_at),
            });
        };

        let fold_id = (fold.started, fold.direction);
        if self.kitty_cursor_placement.seen_fold != Some(fold_id) {
            self.kitty_cursor_placement.seen_fold = Some(fold_id);
            self.kitty_cursor_placement.active = Some(KittyCursorActiveFold {
                started: fold.started,
                direction: fold.direction,
                from: self.kitty_cursor_placement.last_target.unwrap_or(target),
                from_sprite: self.kitty_cursor_placement.last_sprite,
            });
        }
        self.kitty_cursor_placement.last_target = Some(target);

        let Some(active) = self
            .kitty_cursor_placement
            .active
            .filter(|active| active.started == fold.started && active.direction == fold.direction)
        else {
            return Some(KittyCursorPlacement {
                sample: target,
                leg: KittyCursorPlacementLeg::Settled,
                facing_left: motion.facing_left,
                sampled_at: Some(motion.sampled_at),
            });
        };
        let progress = if fold.progress.is_finite() {
            fold.progress.clamp(0.0, 1.0)
        } else {
            self.kitty_cursor_placement.active = None;
            return Some(KittyCursorPlacement {
                sample: target,
                leg: KittyCursorPlacementLeg::Settled,
                facing_left: motion.facing_left,
                sampled_at: Some(motion.sampled_at),
            });
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
            sample: target,
            leg,
            facing_left: motion.facing_left,
            sampled_at: Some(motion.sampled_at),
        })
    }

    /// Re-anchor the cursor kitty after the host detects a coordinate-space tear
    /// between effect sampling and the presented frame. Only pixel continuity
    /// is dropped; art caches and lifecycle state stay.
    pub fn rebase_kitty_cursor_placement(&mut self) {
        self.kitty_cursor_placement = KittyCursorPlacementState::default();
    }

    /// The companion's body height in cells, before the age scale.
    ///
    /// [`crate::cat_baker::CatBaker`] bakes into `2·cell_h` slots and
    /// [`authored_cat_size`] clamps to that. The age axis multiplies this by
    /// 0.82…1.15 ([`crate::genome::CatAge::scale`]), so the base must stay at or
    /// under `2 / 1.15 = 1.739` for the four ages to stay distinct; 1.70 uses
    /// nearly all of it (Elder = 1.955 cells).
    const COMPANION_CELL_H: f32 = 1.70;

    /// The companion's lead ahead of the cursor cell's right edge:
    /// `cell_w · NUM / DEN` = 3/4 cell. See [`Self::kitty_cursor_footprint`].
    const KITTY_LEAD_NUM: i32 = 3;
    const KITTY_LEAD_DEN: i32 = 4;

    /// The flying head's lead in px for a `cell_w` cell — the one number the
    /// footprint and the companion router's seat law both read.
    #[must_use]
    pub fn kitty_cursor_lead_px(cell_w: u16) -> i32 {
        i32::from(cell_w).saturating_mul(Self::KITTY_LEAD_NUM) / Self::KITTY_LEAD_DEN
    }

    /// Time constant, in seconds, of the settled placement follower: each
    /// settled frame the drawn body closes `1 - exp(-dt/PLACE_TAU)` of its gap
    /// to the bob-excluded rest target, smoothing the per-key x hop and the
    /// margin-rise staircase (Appendix A of DESIGN-kitty-motion-2026-08-19).
    /// Quicker than the pose's `kitty_cursor::DISP_TAU` (0.13 s) so the body
    /// escorts the caret rather than trailing it: at 16 ms frames a `cell_h/2`
    /// step lands as ~2 px moves, and a one-cell hop completes in ~0.3 s.
    const PLACE_TAU: f32 = 0.08;

    /// Emit the cat flying in front of the cursor, with the `rainbow kitty`
    /// ribbon as its wake. The lifecycle lives host-side
    /// ([`crate::kitty_cursor::CursorCat`]); this bakes the CatBaker cat or a
    /// user sprite into the shared free atlas and stamps one companion-body
    /// `OverText`/NEAREST [`FreeSprite`] at [`Self::kitty_cursor_footprint`],
    /// with the sing-along notes behind it. Primes the baker itself; a non-open
    /// eye frame that cannot bake falls back to the resident Open-eyes tile.
    /// Call after `tick`, into the same `free` buffer. The settled, clockless
    /// path; hosts that fold use [`Self::kitty_cursor_at_placement`].
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

    /// Emit one cursor kitty at the placement from
    /// [`Self::resolve_kitty_cursor_placement`]. Hosts resolve once, sample
    /// palette from `placement.sample`, and pass the same value here.
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
        // One-body law ([`Self::companion_body_drawn`]): if the pet already
        // drew a body this frame, the head (and its notes) is refused.
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
        // Resolve the atlas tile: a user sprite (resampled, host-baked)
        // overrides the built-in CatBaker cat (happy face, or a wink on the
        // star-wink exit).
        let (ax, ay, w, h) = if let Some(cust) = self.kitty_custom.as_mut() {
            let (nw, nh) = (usize::from(cust.w).max(1), usize::from(cust.h).max(1));
            if cust.cache.as_ref().map(|(cw, chh, _)| (*cw, *chh)) != Some((w, h)) {
                let r = crate::kitty_cursor::resample_nearest(
                    &cust.rgba,
                    nw,
                    nh,
                    usize::from(w),
                    usize::from(h),
                );
                cust.cache = Some((w, h, r));
            }
            let host = crate::kitty_cursor::HOST_ID ^ self.kitty_gen;
            let Some(tile) =
                self.cat_baker
                    .host_tile(host, w, h, &cust.cache.as_ref().expect("set above").2)
            else {
                return None; // budget spent this frame; lands next frame
            };
            (tile.ax, tile.ay, w, h)
        } else {
            // Built-in: the collected character identity. The generated
            // fixed-point aspect keeps reference proportions; palette, age and
            // accessory are scalar bake-key axes.
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
                // The baked blink/squint frame (pose scale/lean is a draw-time
                // transform, never a bake key).
                eyes: pose.eyes,
            };
            // When the exact eyes-frame tile cannot bake this frame (budget
            // spent), fall back to the resident Open-eyes tile so a blink never
            // flickers the cat out; the probe is a pure cache hit and the
            // desired frame lands next frame.
            let tile = self.cat_baker.get_v4(&key).or_else(|| {
                (key.eyes != EyesFrame::Open)
                    .then(|| {
                        self.cat_baker.get_v4(&BakeKeyV4 {
                            eyes: EyesFrame::Open,
                            ..key
                        })
                    })
                    .flatten()
            });
            let Some(tile) = tile else {
                return None; // nothing resident at all; lands next frame
            };
            (tile.ax, tile.ay, w, h)
        };
        // The tile is baked at natural size `w×h` (a stable atlas key); the
        // squash/stretch scales the dest rect about the centre and the lean
        // shifts it, at draw only. `aw/ah` stay the source size, so the
        // renderer NEAREST-scales the fixed tile.
        let (nat_w, nat_h) = (w, h);
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
        let target_x = (cx - i32::from(dest_w) / 2 + lead_px).clamp(0, grid_w - i32::from(dest_w));
        let target_y = cy - i32::from(dest_h) / 2;
        let clearance = i32::from(dest_w)
            .max(i32::from(nat_w))
            .saturating_add(i32::from(geom.cell_w).saturating_mul(2));
        let lift = |q: f32| (0.18 * f32::from(geom.cell_h) * 4.0 * q * (1.0 - q)).round() as i32;
        let lerp_px = |a: i32, b: i32, q: f32| {
            (f64::from(a) + (f64::from(b) - f64::from(a)) * f64::from(q))
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        let (sprite_x, sprite_y) = match placement.leg {
            // Settled leg: one exponential rest-position follower
            // ([`Self::PLACE_TAU`]) smooths the per-key x hop, margin-rise steps,
            // the seat's lift and unauthenticated wraps. The target excludes the
            // hover bob (re-added after, keeping full amplitude and phase). It snaps
            // instead when continuity is dead (first emission, or a gap at or
            // past `CURSOR_FOLD_DURATION`) or the target jumped more than two
            // rows.
            KittyCursorPlacementLeg::Settled => match placement.sampled_at {
                // The clockless one-shot path (`Self::kitty_cursor`, settings
                // preview) draws the raw placement and never touches the
                // follower.
                None => (target_x, target_y),
                Some(at) => {
                    let ch = f32::from(geom.cell_h);
                    let bob_px = (bob * ch).round() as i32;
                    let rest = (target_x as f32, (target_y - bob_px) as f32);
                    let prev_at = self.kitty_cursor_placement.last_emitted_at;
                    let fresh = prev_at.is_some_and(|prev| {
                        at.saturating_duration_since(prev)
                            < crate::kitty_cursor::CURSOR_FOLD_DURATION
                    });
                    // Seeding from the last emitted body makes a post-fold
                    // landing glide in like a mid-line keystroke.
                    let followed = self
                        .kitty_cursor_placement
                        .settle
                        .or_else(|| {
                            fresh
                                .then_some(self.kitty_cursor_placement.last_sprite)
                                .flatten()
                                .map(|(sx, sy)| (sx as f32, (sy - bob_px) as f32))
                        })
                        .filter(|p| fresh && (rest.1 - p.1).abs() <= 2.0 * ch);
                    match followed {
                        Some((mut px, mut py)) => {
                            // dt as in `kitty_cursor::advance_spine`: clamped so
                            // an occluded gap moves one 100 ms step.
                            let dt = prev_at
                                .map(|prev| at.saturating_duration_since(prev).as_secs_f32())
                                .unwrap_or(0.0)
                                .min(0.1);
                            let alpha = 1.0 - (-dt / Self::PLACE_TAU).exp();
                            px += (rest.0 - px) * alpha;
                            py += (rest.1 - py) * alpha;
                            self.kitty_cursor_placement.settle = Some((px, py));
                            (px.round() as i32, py.round() as i32 + bob_px)
                        }
                        None => {
                            self.kitty_cursor_placement.settle = Some(rest);
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
                // A fold owns the body for both half-beats; the settled
                // follower re-seeds from the landing's emitted sprite.
                self.kitty_cursor_placement.settle = None;
                let from_cx = from.x + i32::from(nat_w) / 2;
                let from_x = (from_cx - i32::from(dest_w) / 2 + lead_px)
                    .clamp(0, grid_w - i32::from(dest_w));
                let from_y = from.y + i32::from(nat_h) / 2 - i32::from(dest_h) / 2;
                let (start_x, start_y) = from_sprite.unwrap_or((from_x, from_y));
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
                    lerp_px(start_y, from_y, eased).saturating_sub(lift(eased)),
                )
            }
            KittyCursorPlacementLeg::Enter { direction, eased } => {
                self.kitty_cursor_placement.settle = None;
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
                    target_y.saturating_sub(lift(eased)),
                )
            }
        };
        let sprite = FreeSprite {
            x: sprite_x,
            y: sprite_y,
            w: dest_w,
            h: dest_h,
            ax,
            ay,
            aw: nat_w,
            ah: nat_h,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x,
            z: FreeZ::OverText,
            sampler: FreeSampler::Nearest,
        };

        // ── the sing-along's ♪/♫ music notes (`crate::kitty_sing`) ──────────
        // Streaming from the cat's mouth, pushed before the body so a new note
        // slides out from behind the head. Cursor-cat-only, capped at
        // `kitty_sing::MAX_NOTES`; two white-baked tiles are rainbow-tinted per
        // sprite via `FreeSprite::tint`. A note that misses the bake budget
        // lands next frame.
        let mut note_sprites: [Option<FreeSprite>; crate::kitty_sing::MAX_NOTES] =
            [None; crate::kitty_sing::MAX_NOTES];
        if sing > 0.0 {
            let grid_w = i32::from(geom.cols).saturating_mul(i32::from(geom.cell_w));
            let mouth_x = if flip_x {
                sprite.x
            } else {
                sprite.x + i32::from(sprite.w)
            };
            let mouth_y = sprite.y + i32::from(sprite.h) / 2;
            let note_dir = if flip_x { -1.0 } else { 1.0 };
            for (slot, note) in note_sprites.iter_mut().zip(notes.iter().flatten()) {
                let kind_idx = match note.kind {
                    crate::kitty_sing::NoteKind::Eighth => 0usize,
                    crate::kitty_sing::NoteKind::Beamed => 1,
                };
                let (nw, nh) = crate::kitty_sing::note_nat_size(note.kind, geom.cell_h);
                let host = crate::kitty_sing::note_host_id(note.kind, nw, nh);
                let Some(tile) = NotePaintCache::tile(
                    &mut self.note_cache[kind_idx],
                    &mut self.cat_baker,
                    host,
                    (nw, nh),
                    || crate::kitty_sing::bake_note(nw, nh, note.kind),
                ) else {
                    continue; // budget spent; this note lands next frame
                };
                // The wind-down crossfade rides the alpha: note envelope ×
                // cat presentation × sing drive.
                let a = (f32::from(note.alpha) * f32::from(alpha) / 255.0 * sing) as u8;
                if a == 0 {
                    continue;
                }
                let nx = mouth_x + (note_dir * note.dx * f32::from(geom.cell_w)).round() as i32
                    - i32::from(nw) / 2;
                let ny =
                    mouth_y + (note.dy * f32::from(geom.cell_h)).round() as i32 - i32::from(nh) / 2;
                if i32::from(nw) > grid_w {
                    continue; // a grid narrower than one note sings unadorned
                }
                *slot = Some(FreeSprite {
                    // Settled notes stay on-grid. During a fold they keep their
                    // signed coordinate; clamping would pin them to the opposite
                    // edge while the body is off glass.
                    x: if placement.in_motion() {
                        nx
                    } else {
                        nx.clamp(0, grid_w - i32::from(nw))
                    },
                    y: ny,
                    w: nw,
                    h: nh,
                    ax: tile.ax,
                    ay: tile.ay,
                    aw: nw,
                    ah: nh,
                    tint: note.tint,
                    alpha: a,
                    flip_x: false,
                    z: FreeZ::OverText,
                    sampler: FreeSampler::Nearest,
                });
            }
        }

        for note in note_sprites.iter().flatten() {
            free.push(*note);
        }
        free.push(sprite);
        if let Some(at) = placement.sampled_at {
            self.kitty_cursor_placement.last_sprite = Some((sprite.x, sprite.y));
            self.kitty_cursor_placement.last_emitted_at = Some(at);
        }

        // `tick` folds the atlas version before this post-tick bake. Return a
        // complete cursor-art fingerprint so the host's early-out sees a fresh
        // atlas upload or a local-palette change.
        let mut fp = fold_free(FNV64_OFFSET, &sprite);
        // The notes fold too: a rising/fading note (or a deferred note tile
        // landing) must never be swallowed by a host early-out.
        for note in note_sprites.iter().flatten() {
            fp = fold_free(fp, note);
        }
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    /// Claim this host frame's one companion body, or refuse
    /// ([`Self::companion_body_drawn`]). Taken before baking: over-claiming on
    /// a frame whose bake misses costs nothing.
    fn claim_companion_body(&mut self) -> bool {
        if !self.host_brackets_frames {
            return true;
        }
        if self.companion_body_drawn {
            return false;
        }
        self.companion_body_drawn = true;
        true
    }

    /// Emit the full-body pet ([`crate::kitty_pet`]) as one free sprite, at the
    /// brain's fractional column with its feet on `(row + 1)·cell_h` (lifted by
    /// the arc; squash and stretch about the feet), plus its motes and the
    /// breed handoff's departing bodies, each in its own look and held tile.
    /// `None` when the pet is invisible or another companion body owns the
    /// frame; a frame whose tiles have not baked still returns its fingerprint.
    pub fn pet_cursor(&mut self, frame: PetCursorFrame, free: &mut Vec<FreeSprite>) -> Option<u64> {
        let PetCursorFrame {
            geom,
            colors,
            coat,
            iris,
            pet,
            page,
        } = frame;
        // Keyed on presence (`lane_alpha`), not the composed body alpha: the
        // arrival multiply can round a faint body's `alpha` to 0 while its
        // ghosts are still live. `lane_alpha == 0` implies `alpha == 0`.
        if pet.lane_alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        // The one-body law ([`Self::companion_body_drawn`]): the frame's one
        // companion body, or nothing.
        if !self.claim_companion_body() {
            return None;
        }
        self.begin_baker_frame(geom, true);
        self.drop_holds_if_metric_moved(geom.cell_w, geom.cell_h);
        self.pet_baker.begin_frame(geom.cell_w, geom.cell_h);

        // Natural size from the authored art height. `host_tile` requires
        // `w <= 4·cell_h` and `h <= 2·cell_h`; the roster's 1.70-row height and
        // 1.706 aspect fit (asserted by the pet-art quality test).
        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(geom.cell_h)).round();
        let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
        let nat_h = (nat_h as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let nat_w = (nat_w as i32).clamp(1, i32::from(u16::MAX)) as u16;

        let key = crate::pet_baker::PetBakeKey {
            pose: pet.pose,
            coat,
            iris,
            colors,
            w: nat_w,
            h: nat_h,
        };
        // `host_id` carries its own high-bit tag, so a pet tile cannot alias
        // the kitty-cursor sprite's host id.
        let host = key.host_id();
        // Two doors, one budget: bake the pose into the pet's exact-size cache,
        // then hand the texels to the shared atlas. `tick` has first claim on
        // the two-bake budget, so a miss is ordinary while walking; the last
        // resolved pose is re-emitted instead of blinking out, and the new one
        // lands next frame. The hold is invalidated with the atlas.
        let resolved = self
            .pet_baker
            .tile(&key)
            .and_then(|rgba| self.cat_baker.host_tile(host, nat_w, nat_h, rgba))
            .map(|t| (t.ax, t.ay, key.pose, coat, iris));
        if let Some(r) = resolved {
            self.pet_last_tile = Some(r);
        }
        // Look-keyed fallback: a held tile in a different `(coat, iris)` is
        // refused. On a handoff swap frame under bake pressure the live body
        // skips a frame (its old-look ghost already stands there) rather than
        // showing two identical cats. Motes and departing bodies draw anyway.
        let body_tile = resolved.or_else(|| {
            self.pet_last_tile
                .filter(|&(.., held_coat, held_iris)| (held_coat, held_iris) == (coat, iris))
        });
        let sprite = body_tile.and_then(|(ax, ay, ..)| {
            // Dest rect about the feet, from
            // [`crate::kitty_pet::PetFrame::body_px`] — the same rect hosts pass
            // to `tick` as [`CompanionOnGlass::body_px`], so word-cats yield to
            // the body actually drawn.
            let (x0, x1, y0, y1) = pet.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)?;
            Some(FreeSprite {
                x: x0,
                y: y0,
                w: (x1 - x0).clamp(1, i32::from(u16::MAX)) as u16,
                h: (y1 - y0).clamp(1, i32::from(u16::MAX)) as u16,
                ax,
                ay,
                aw: nat_w,
                ah: nat_h,
                tint: 0x00FF_FFFF,
                alpha: pet.alpha,
                flip_x: pet.facing_left,
                // Hiding: the pet drops under the glyphs, so the word stays
                // readable in front with the ears poking above the line.
                z: if pet.under_ink {
                    FreeZ::UnderText
                } else {
                    FreeZ::OverText
                },
                sampler: FreeSampler::Nearest,
            })
        });

        // ── the pet's mote lane ─────────────────────────────────────────────
        // Landing dust, sleep z's, purr notes. Never grid-aligned or
        // text-coloured, so none reads as a terminal glyph; white tiles at a few
        // rotation buckets, tinted per sprite. Pushed before the body, so a
        // fresh mote emerges from behind the cat.
        let mut mote_sprites: [Option<FreeSprite>; crate::kitty_pet::PET_MOTES_MAX] =
            [None; crate::kitty_pet::PET_MOTES_MAX];
        // Held inside the grid on both axes ([`crate::kitty_pet::mote_box_px`]):
        // a mote born at the pet's `head_anchor` (~0.6 rows above its row)
        // would have negative y on row 0, and the tab-strip splice shifts free
        // sprites down into the strip, painting over the tab titles.
        for (slot, m) in mote_sprites.iter_mut().zip(pet.motes.iter().flatten()) {
            // `pet.alpha` (presence × arrival), not `lane_alpha`: motes are the
            // live body's accents and dim with it. Ghosts own no motes.
            let alpha = (f32::from(m.alpha) * f32::from(pet.alpha) / 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let side = crate::kitty_pet::mote_tile_side(geom.cell_h);
            let bucket = Self::pet_mote_rot_bucket(m.rot);
            let host = Self::pet_mote_host_id(m.kind, bucket, side);
            // Cache first: `host_peek` is `host_tile`'s hit branch (same lookup
            // and LRU touch), so a warm mote tile is not re-rasterized every
            // frame. Only a miss runs `bake_pet_mote` and spends bake budget;
            // the bake is a pure function of the host-id triple.
            let Some(tile) = self.cat_baker.host_peek(host).or_else(|| {
                let paint = Self::bake_pet_mote(m.kind, bucket, side);
                self.cat_baker.host_tile(host, side, side, paint.pixels())
            }) else {
                continue; // budget spent; this mote lands next frame
            };
            // The box the brain's ♪/♥ lanes are judged by
            // ([`crate::kitty_pet::mote_box_px`]): one placement, two readers.
            let Some((x, y, dest)) = crate::kitty_pet::mote_box_px(
                (m.col, m.row),
                m.scale,
                (geom.cell_w, geom.cell_h),
                (geom.cols, geom.rows),
            ) else {
                continue;
            };
            *slot = Some(FreeSprite {
                x,
                y,
                w: dest,
                h: dest,
                ax: tile.ax,
                ay: tile.ay,
                aw: side,
                ah: side,
                // A ♪/♥ born over a lit ribbon wears the ribbon colour offered
                // at that cell ([`crate::kitty_pet::PetMoteSprite::rgb`]),
                // already snapped to one of the seven stops; other motes keep
                // the per-kind tint. On paper either takes the mark's ink.
                tint: Self::pet_mote_on_page(
                    m.kind,
                    m.rgb.unwrap_or_else(|| Self::pet_mote_tint(m.kind, colors)),
                    page,
                ),
                alpha,
                flip_x: false,
                z: FreeZ::OverText,
                sampler: FreeSampler::Nearest,
            });
        }
        // ── the departing bodies (the breed handoff's outgoing kitties) ────
        // Each ghost draws like the live body (its own `PetBakeKey`, the same
        // feet-anchored `body_px`) in its old `(coat, iris)` and palette; held
        // tiles absorb bake misses. Ghosts push first, so they run off behind
        // the live pet.
        let mut depart_sprites: [Option<FreeSprite>; crate::kitty_pet::PET_DEPARTURES_MAX] =
            [None; crate::kitty_pet::PET_DEPARTURES_MAX];
        for (slot, d) in depart_sprites.iter_mut().zip(pet.departures) {
            // A ghost on its own glass stands in another pane's cells
            // ([`Self::pet_own_glass`] draws it); its held tile stays.
            let Some(d) = d.filter(|d| !d.own_glass) else {
                continue;
            };
            *slot = self.pet_departure_sprite(d, &pet, geom, colors, (nat_w, nat_h));
        }
        for g in depart_sprites.iter().flatten() {
            free.push(*g);
        }
        for m in mote_sprites.iter().flatten() {
            free.push(*m);
        }
        if let Some(s) = sprite {
            free.push(s);
        }

        let mut fp = FNV64_OFFSET;
        if let Some(s) = &sprite {
            fp = fold_free(fp, s);
        }
        // Motes fold too: a drifting mote over a still sleeper may be the only
        // change on glass.
        for m in mote_sprites.iter().flatten() {
            fp = fold_free(fp, m);
        }
        // And the departing bodies, for the same reason: a ghost sprinting
        // off beside a settled pet is often the only change on glass.
        for g in depart_sprites.iter().flatten() {
            fp = fold_free(fp, g);
        }
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, self.pet_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    /// Hold `at` as ghost `id`'s tile (`pet_depart_tiles`): in its own entry,
    /// else in one that no ghost of this frame (`live`) owns — there is always
    /// one, since the table is twice the lane and ids are unique.
    fn hold_departure_tile(
        &mut self,
        id: u32,
        (ax, ay): (u16, u16),
        live: &[Option<crate::kitty_pet::PetDepartureSprite>],
    ) {
        let owned = |held: u32| live.iter().flatten().any(|d| d.id == held);
        let entry = self
            .pet_depart_tiles
            .iter()
            .position(|e| e.is_some_and(|(held, ..)| held == id))
            .or_else(|| {
                self.pet_depart_tiles
                    .iter()
                    .position(|e| e.is_none_or(|(held, ..)| !owned(held)))
            });
        if let Some(entry) = entry {
            self.pet_depart_tiles[entry] = Some((id, ax, ay));
        }
    }

    /// One departing body's sprite at `geom`, in that grid's pixels, shared by
    /// [`Self::pet_cursor`] and [`Self::pet_own_glass`]. Baked in the ghost's
    /// own palette, else `colors`; its id keys its held tile.
    fn pet_departure_sprite(
        &mut self,
        d: crate::kitty_pet::PetDepartureSprite,
        pet: &crate::kitty_pet::PetFrame,
        geom: EffectGeom,
        colors: CatColorKey,
        (nat_w, nat_h): (u16, u16),
    ) -> Option<FreeSprite> {
        // The ghost rides the live pet's presence envelope (`lane_alpha`) but
        // never the arrival ramp, which belongs to the incoming body: the
        // composed `pet.alpha` would launch the leaver at 6% beside a 6%
        // arriver on the swap frame.
        let alpha = (u16::from(d.alpha) * u16::from(pet.lane_alpha) / 255) as u8;
        if alpha == 0 {
            return None;
        }
        // The ghost's own hold, found by its id, never by its slot.
        let held = self
            .pet_depart_tiles
            .iter()
            .flatten()
            .find(|&&(id, ..)| id == d.id)
            .map(|&(_, ax, ay)| (ax, ay));
        let key = crate::pet_baker::PetBakeKey {
            pose: d.pose,
            coat: d.coat,
            iris: d.iris,
            // The palette its body was drawn in: a ghost is that body,
            // colours included (`PetDepartureSprite::colors`).
            colors: d.colors.unwrap_or(colors),
            w: nat_w,
            h: nat_h,
        };
        let resolved = self
            .pet_baker
            .tile(&key)
            .and_then(|rgba| self.cat_baker.host_tile(key.host_id(), nat_w, nat_h, rgba))
            .map(|t| (t.ax, t.ay));
        if let Some(at) = resolved {
            self.hold_departure_tile(d.id, at, &pet.departures);
        }
        // Budget spent, nothing held yet: lands next frame.
        let (ax, ay) = resolved.or(held)?;
        let ghost = crate::kitty_pet::PetFrame {
            alpha,
            // The ghost's lane alpha is its composed byte: `body_px` and the
            // sprite read the same value, and the lane factor is already
            // folded in above.
            lane_alpha: alpha,
            action: pet.action,
            pose: d.pose,
            col: d.col,
            row: d.row,
            // A still ghost at the stance the body was last drawn at, so it
            // is that body to the pixel; one running off at the authored
            // size on the ground (`PetDepartureSprite::scale_x`).
            lift: d.lift,
            facing_left: d.facing_left,
            scale_x: d.scale_x,
            scale_y: d.scale_y,
            purr: 0.0,
            contented: false,
            // The ghost stands where the body stood; it wears the body's
            // z-order, so a cat that was peering out from behind a word does
            // not spend its last quarter second painted over it.
            under_ink: pet.under_ink,
            motes: [None; crate::kitty_pet::PET_MOTES_MAX],
            departures: [None; crate::kitty_pet::PET_DEPARTURES_MAX],
        };
        let (x0, x1, y0, y1) = ghost.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)?;
        Some(FreeSprite {
            x: x0,
            y: y0,
            w: (x1 - x0).clamp(1, i32::from(u16::MAX)) as u16,
            h: (y1 - y0).clamp(1, i32::from(u16::MAX)) as u16,
            ax,
            ay,
            aw: nat_w,
            ah: nat_h,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x: d.facing_left,
            // A re-sighting ghost stands on glass a new screen owns, or on its
            // own unobserved glass (`PetDepartureSprite::under_ink`): beneath
            // the ink, so the text stays legible.
            z: if d.under_ink {
                FreeZ::UnderText
            } else {
                FreeZ::OverText
            },
            sampler: FreeSampler::Nearest,
        })
    }

    /// Draw the departure in `slot` on its own glass
    /// (`PetDepartureSprite::own_glass`): a split-pane focus switch's fading
    /// half, the old body still in the pane the focus left. It is drawn at that
    /// pane's grid (`own`: its cols and rows at this frame's cell metrics),
    /// moved to the pane's window-grid `origin` and cropped to it, beneath the
    /// text. Pushed into `free`, the window's channel: a pane's placement would
    /// put it in the focused pane. One call per ghost (`CompanionFrame::glass`).
    ///
    /// Call it right after a [`Self::pet_cursor`] that drew this frame's pet
    /// (`Some`): it rides that call's baker frame, one-body claim and
    /// presence envelope. Returns the fingerprint of what it drew.
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
        // The cell metrics are the frame's: `own` is `geom` with the ghost's
        // pane's grid. A ghost whose pane is no longer at these metrics was
        // dropped before this frame was ticked (`CompanionOwner::tick`).
        if pet.lane_alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
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
        let mut fp = FNV64_OFFSET;
        let mut drew = false;
        for (i, d) in pet.departures.iter().enumerate() {
            let Some(d) = d.filter(|d| d.own_glass && i == slot) else {
                continue;
            };
            let Some(mut sprite) = self.pet_departure_sprite(d, &pet, own, colors, nat) else {
                continue;
            };
            sprite.x = sprite.x.saturating_add(origin.0);
            sprite.y = sprite.y.saturating_add(origin.1);
            if !crate::pane::crop_free_sprite(&mut sprite, pane) {
                continue;
            }
            fp = fold_free(fp, &sprite);
            free.push(sprite);
            drew = true;
        }
        drew.then_some(fp)
    }

    /// The pet's ink probe, for hosts: `(spans, live_row)`, where `spans[r]` is
    /// viewport row `r`'s inked columns as a half-open `[first, end)` span
    /// (`(0, 0)` = blank) and `live_row` is the newest inked row on glass, the
    /// output edge the pet's watch hugs. Refreshed by every rescan from the row
    /// walk that feeds the lexicon.
    ///
    /// Pass both to [`crate::kitty_pet::PetBrain::sense_ink`]
    /// (`sense_ink(0, spans, live_row)`) just before that brain's `tick`.
    #[must_use]
    pub fn pet_ink(&self) -> (&[(u16, u16)], Option<u16>) {
        let live = self
            .pet_ink_spans
            .iter()
            .rposition(|&(first, end)| end > first)
            .map(|r| r as u16);
        (&self.pet_ink_spans, live)
    }

    // ── pet mote paint ──────────────────────────────────────────────────────
    // Mote rasterizer: white-baked, rotation baked at PET_MOTE_ROT_BUCKETS
    // steps (FreeSprite has no rotation), tinted per sprite.

    /// Host-id family salt for pet mote tiles — its own family, splitmix-
    /// scrambled away from the kitty-sprite, music-note and pet-pose ids.
    const PET_MOTE_HOST_SALT: u64 = 0x7A3D_51C9_E60B_2F81;
    /// Rotation buckets per full turn: enough that a spinning mote visibly
    /// turns, few enough that the atlas holds a handful of tiny tiles.
    const PET_MOTE_ROT_BUCKETS: u32 = 12;
    /// The pet's accent palette, indexed by [`CatColorKey::accent`] (the hues
    /// `cat_baker`'s context accents resolve; `12` is neutral). Mote tints
    /// derive from these, never from text colours.
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

    /// Quantize a rotation to its bake bucket.
    fn pet_mote_rot_bucket(rot: f32) -> u32 {
        let turn = rot.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
        ((turn * Self::PET_MOTE_ROT_BUCKETS as f32) as u32) % Self::PET_MOTE_ROT_BUCKETS
    }

    fn pet_mote_kind_tag(kind: crate::kitty_pet::PetMoteKind) -> u64 {
        match kind {
            crate::kitty_pet::PetMoteKind::Dust => 1,
            // The z and its startled pop share one tile family: same art,
            // different flight.
            crate::kitty_pet::PetMoteKind::Zee | crate::kitty_pet::PetMoteKind::ZeePop => 2,
            crate::kitty_pet::PetMoteKind::Note => 3,
            crate::kitty_pet::PetMoteKind::Heart => 4,
        }
    }

    /// Stable atlas identity for one mote tile: kind, rotation bucket and exact
    /// size. Tint is absent: tiles bake white and are tinted per sprite.
    fn pet_mote_host_id(kind: crate::kitty_pet::PetMoteKind, bucket: u32, side: u16) -> u64 {
        let mut x = Self::PET_MOTE_HOST_SALT
            ^ (Self::pet_mote_kind_tag(kind) << 40)
            ^ (u64::from(bucket) << 20)
            ^ u64::from(side);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }

    /// Per-kind tint, never a text colour. Dust is a neutral puff warmed by the
    /// accent; z's float pale. The note is gold and the heart pink on every
    /// coat and ground, with only a breath of the episode accent.
    fn pet_mote_tint(kind: crate::kitty_pet::PetMoteKind, colors: CatColorKey) -> u32 {
        let accent = Self::PET_MOTE_ACCENTS[usize::from(colors.accent.min(12))];
        match kind {
            crate::kitty_pet::PetMoteKind::Dust => mix_rgb(0x00B9_B9C0, accent, 0.22),
            crate::kitty_pet::PetMoteKind::Zee | crate::kitty_pet::PetMoteKind::ZeePop => {
                mix_rgb(0x00F0_F0F6, accent, 0.45)
            }
            crate::kitty_pet::PetMoteKind::Note => mix_rgb(0x00FF_C24D, accent, 0.15),
            crate::kitty_pet::PetMoteKind::Heart => mix_rgb(0x00FF_6B9A, accent, 0.15),
        }
    }

    /// THE PAPER LAW FOR THE PET'S ♪ AND ♥: on a light page each keeps its
    /// hue at the point mark's contrast
    /// ([`crate::rainbow_kitty::ribbon::paper_mark_ink`], 4.5:1 against the
    /// page), so a cheer's gold note reads as amber and its pink heart as rose
    /// on white or cream, never as a pale smudge. Dust and the z's are
    /// neutral and keep their tint (the law bends a hue, and a grey has
    /// none); a dark ground changes nothing.
    fn pet_mote_on_page(kind: crate::kitty_pet::PetMoteKind, tint: u32, page: Option<u32>) -> u32 {
        match (kind, page) {
            (
                crate::kitty_pet::PetMoteKind::Note | crate::kitty_pet::PetMoteKind::Heart,
                Some(bg),
            ) => crate::rainbow_kitty::ribbon::paper_mark_ink(tint, bg),
            _ => tint,
        }
    }

    /// Bake one mote tile, white, at its rotation bucket: fixed drawlists
    /// rotated about the tile centre, deterministic per `(kind, bucket, side)`.
    fn bake_pet_mote(
        kind: crate::kitty_pet::PetMoteKind,
        bucket: u32,
        side: u16,
    ) -> aterm_scene::Tile {
        let mut tile = aterm_scene::Tile::new(u32::from(side), u32::from(side));
        if side == 0 {
            return tile;
        }
        let s = f32::from(side);
        let ang = bucket as f32 / Self::PET_MOTE_ROT_BUCKETS as f32 * std::f32::consts::TAU;
        let (sin, cos) = ang.sin_cos();
        // Rotate a glyph-frame point about the tile centre.
        let rp = move |u: f32, v: f32| -> (f32, f32) {
            let (du, dv) = (u - 0.5, v - 0.5);
            (0.5 + du * cos - dv * sin, 0.5 + du * sin + dv * cos)
        };
        let white = (1.0, 1.0, 1.0);
        // Rotate a 0..1 polygon about the tile centre into a fillable path.
        let rot_poly = |pts: &[(f32, f32)]| -> Vec<aterm_scene::PathCmd> {
            let mut cmds = Vec::with_capacity(pts.len() + 1);
            for (i, &(u, v)) in pts.iter().enumerate() {
                let (x, y) = rp(u, v);
                cmds.push(if i == 0 {
                    aterm_scene::PathCmd::Move(x, y)
                } else {
                    aterm_scene::PathCmd::Line(x, y)
                });
            }
            cmds.push(aterm_scene::PathCmd::Close);
            cmds
        };
        let fit = aterm_scene::PathTransform::fit(u32::from(side), u32::from(side));
        match kind {
            crate::kitty_pet::PetMoteKind::Dust => {
                // An asymmetric puff — three soft discs, so the baked
                // rotation actually reads and no silhouette resembles type.
                for &(u, v, r, a) in &[
                    (0.42f32, 0.52f32, 0.26f32, 0.50f32),
                    (0.68, 0.44, 0.16, 0.42),
                    (0.30, 0.34, 0.12, 0.36),
                ] {
                    let (x, y) = rp(u, v);
                    tile.disc(x * s, y * s, (r * s).max(1.0), white, a);
                }
            }
            crate::kitty_pet::PetMoteKind::Zee | crate::kitty_pet::PetMoteKind::ZeePop => {
                // The z: one polygon, always tilted (the brain's rotation is
                // nonzero), scaling and fading as it drifts.
                let z = rot_poly(&[
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
                ]);
                aterm_scene::fill_path(&mut tile, &[z.as_slice()], white, 0.88, fit);
            }
            crate::kitty_pet::PetMoteKind::Note => {
                // ♪ — stem, a small flag, and a round head at its foot.
                let stem = rot_poly(&[(0.46, 0.12), (0.58, 0.12), (0.58, 0.72), (0.46, 0.72)]);
                let flag = rot_poly(&[(0.58, 0.12), (0.82, 0.24), (0.74, 0.44), (0.58, 0.28)]);
                aterm_scene::fill_path(&mut tile, &[stem.as_slice()], white, 0.88, fit);
                aterm_scene::fill_path(&mut tile, &[flag.as_slice()], white, 0.88, fit);
                let (hx, hy) = rp(0.42, 0.78);
                tile.disc(hx * s, hy * s, (0.15 * s).max(1.2), white, 0.88);
            }
            crate::kitty_pet::PetMoteKind::Heart => {
                // Two lobes and a point — discs for the round tops, one
                // triangle for the tip.
                let tip = rot_poly(&[(0.18, 0.46), (0.82, 0.46), (0.50, 0.86)]);
                aterm_scene::fill_path(&mut tile, &[tip.as_slice()], white, 0.85, fit);
                for &(u, v) in &[(0.34f32, 0.38f32), (0.66, 0.38)] {
                    let (x, y) = rp(u, v);
                    tile.disc(x * s, y * s, (0.19 * s).max(1.2), white, 0.85);
                }
            }
        }
        tile
    }

    /// The typed-word dog's trail behind the cursor cell's left edge, as a
    /// fraction of a cell, mirroring the kitty's [`Self::KITTY_LEAD_NUM`] lead:
    /// the kitty escorts ahead of the caret, the dog pops up behind it.
    const DOG_TRAIL_NUM: i32 = 3;
    const DOG_TRAIL_DEN: i32 = 4;

    /// Emit the typed-word dog cameo ([`crate::dog_cameo`]) as one free sprite
    /// just behind the cursor cell, centred on its row. Same two-door bake as
    /// the pet; a miss re-emits the last resolved tile. Near the left margin it
    /// clamps and rises rather than covering the glyph being typed. Call after
    /// `tick`; returns the early-out fingerprint, `None` when nothing drew.
    pub fn dog_cameo(&mut self, frame: DogCameoFrame, free: &mut Vec<FreeSprite>) -> Option<u64> {
        let DogCameoFrame {
            geom,
            cursor: (crow, ccol),
            look,
            colors,
            bob,
            alpha,
        } = frame;
        if alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        self.begin_baker_frame(geom, false);
        self.drop_holds_if_metric_moved(geom.cell_w, geom.cell_h);
        self.dog_baker.begin_frame(geom.cell_w, geom.cell_h);

        // Natural size: 1.45 rows tall (under the ambient cat's 1.7·ch) at this
        // breed's aspect, clamped to the host_tile slot (4·ch × 2·ch).
        let ch = i32::from(geom.cell_h);
        let cw = i32::from(geom.cell_w);
        let aspect = crate::dog_baker::DogBaker::aspect(look.breed);
        let max_w = f32::from(geom.cell_h.saturating_mul(4).max(1));
        let max_h = f32::from(geom.cell_h.saturating_mul(2).max(1));
        let desired_h = 1.45 * f32::from(geom.cell_h);
        let fitted_h = desired_h.max(1.0).min(max_h).min(max_w / aspect);
        let h = (fitted_h.round() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let w = ((f32::from(h) * aspect).round() as i32).clamp(1, i32::from(u16::MAX)) as u16;

        let key = crate::dog_baker::DogBakeKey {
            breed: look.breed,
            coat: look.coat,
            colors,
            w,
            h,
        };
        let resolved = self
            .dog_baker
            .tile(&key)
            .and_then(|rgba| self.cat_baker.host_tile(key.host_id(), w, h, rgba))
            .map(|t| (t.ax, t.ay, look));
        if let Some(r) = resolved {
            self.dog_last_tile = Some(r);
        }
        // Look-keyed fallback ([`Self::pet_last_tile`]): a held tile of another
        // breed is refused. A re-summon rolls a new breed whose first frame is
        // near-zero alpha, so skipping it is invisible.
        let (ax, ay, _) =
            resolved.or_else(|| self.dog_last_tile.filter(|&(.., held)| held == look))?;

        // Trail just behind the cursor cell (`DOG_TRAIL_NUM`). Near the left
        // margin the dog clamps to the grid edge and rises rather than sliding
        // over the cursor cell.
        let grid_w = i32::from(geom.cols).saturating_mul(cw);
        let cursor_left = i32::from(ccol).saturating_mul(cw);
        let trail = cw.saturating_mul(Self::DOG_TRAIL_NUM) / Self::DOG_TRAIL_DEN;
        let x = cursor_left
            .saturating_sub(trail)
            .saturating_sub(i32::from(w))
            .max(0);
        if i32::from(w) > grid_w {
            return None; // the grid is narrower than the dog itself
        }
        let rest_top = i32::from(crow) * ch + ch / 2 - i32::from(h) / 2;
        let intrusion = (x + i32::from(w)) - cursor_left;
        let rise = if intrusion > 0 {
            intrusion.min(ch / 2).min(rest_top.max(0))
        } else {
            0
        };
        let y = rest_top - rise + (bob * ch as f32).round() as i32;
        let sprite = FreeSprite {
            x,
            y,
            w,
            h,
            ax,
            ay,
            aw: w,
            ah: h,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x: false,
            z: FreeZ::OverText,
            sampler: FreeSampler::Nearest,
        };
        free.push(sprite);

        let mut fp = fold_free(FNV64_OFFSET, &sprite);
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, self.dog_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    /// Install one already-resolved immutable source. No filesystem access,
    /// decoding or cloning of custom bytes. The source fingerprint keys the
    /// shared-atlas tile; a target metric change rebuilds only the resample
    /// cache.
    pub fn set_kitty_sprite_source(&mut self, source: KittySpriteSource) {
        // A replacement names different pixels, so a fold begun under the old
        // source must not carry its cached origin over.
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
                    && usize::from(w)
                        .checked_mul(usize::from(h))
                        .and_then(|pixels| pixels.checked_mul(4))
                        == Some(rgba.len());
                if !valid {
                    self.kitty_custom = None;
                    self.kitty_disabled = true;
                    self.kitty_gen = u64::MAX;
                    return;
                }
                self.kitty_custom = Some(KittyCustom {
                    source_fp,
                    w,
                    h,
                    rgba,
                    cache: None,
                });
                self.kitty_disabled = false;
                self.kitty_gen = source_fp;
            }
        }
    }

    /// Diagnosable, allocation-free installed source identity for host
    /// conformance tests and capture/glass generation checks.
    #[doc(hidden)]
    pub fn kitty_sprite_source_fingerprint(&self) -> Option<u64> {
        if self.kitty_disabled {
            None
        } else {
            Some(
                self.kitty_custom
                    .as_ref()
                    .map_or(0, |custom| custom.source_fp),
            )
        }
    }

    /// Borrow the exact immutable RGBA Arc supplied by the host.
    #[doc(hidden)]
    pub fn kitty_sprite_rgba(&self) -> Option<&Arc<[u8]>> {
        self.kitty_custom.as_ref().map(|custom| &custom.rgba)
    }

    /// Whether a user-configured kitty sprite currently overrides the built-in
    /// homage. Exposed for host conformance checks and diagnostics.
    #[must_use]
    pub fn has_custom_kitty_sprite(&self) -> bool {
        self.kitty_custom.is_some()
    }

    /// Record a committed edit keystroke (printable/IME glyph, backspace,
    /// line/word kill) in `pane` (`None` from a host that binds no pane).
    ///
    /// Fed from the host's input path, never PTY output: the caret alone is no
    /// witness of typing (see [`TYPED_EDIT_WITNESS_MS`]).
    ///
    /// Call it for content-changing keys only; navigation (arrows, Home/End)
    /// must not license a re-arm.
    pub fn note_typed_edit(&mut self, now: Instant, pane: Option<u64>) {
        self.typed_edit = Some(TypedEdit {
            at: now,
            // The host holds a key event, not a grid; the engine's last scanned
            // caret for the named pane is the pre-echo caret this key was typed
            // at ([`Self::last_caret`]). It must be the named pane's caret, not
            // the bound one: this runs on the input schedule, and the bound pane
            // is whatever the last composed frame left.
            cell: self.caret_of_pane(pane),
            pane,
            spent: false,
        });
    }

    /// Record that `word` was just typed to the pet in `pane` (the
    /// [`crate::typed_tricks`] listener fired), so the word flashes rainbow
    /// once the rescan finds it. `word` is the raw trick token; `back_chars`
    /// counts the typed characters between it and the caret (0 for `sit␣`, 6
    /// for the `good` of `good kitty␣`).
    ///
    /// Fed from the host input path only, so output and pastes never mint a
    /// flash. The place comes from the pane's pre-echo caret and recorded
    /// base-y. Fail-silent without a caret observation; allocation-free.
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

    /// The typed line was not pet talk after all (`sit tight`): a visible flash
    /// leaves within a tenth of a second instead of finishing, and one not yet
    /// found is dropped. Idempotent.
    pub fn revoke_trick_flash(&mut self, now: Instant) {
        self.trick_flash.revoke(now);
    }

    /// Each feline occurrence's palette key, and whether any occurrence is a
    /// profanity: what a host's scan decided, for the pipeline's tests.
    #[cfg(test)]
    pub(crate) fn scan_verdicts(&self) -> (Vec<CatColorKey>, bool) {
        (
            self.occ
                .iter()
                .filter(|o| o.class == Class::Feline)
                .map(|o| o.cat_colors)
                .collect(),
            self.occ.iter().any(|o| o.class == Class::Profanity),
        )
    }

    /// Where the trick flash stands at `now` (pending, live, revoked or idle),
    /// for tests, the lifecycle model's conformance and driven validation.
    #[must_use]
    pub fn trick_flash_phase(&self, now: Instant) -> TrickFlashPhase {
        self.trick_flash.phase(now)
    }

    /// The last scanned caret belonging to `pane`: the live fields when they are
    /// that pane's (or nothing is bound), its parked slot otherwise. `None`
    /// means no observation: [`Self::typed_edit_reached`] then abstains.
    fn caret_of_pane(&self, pane: Option<u64>) -> Option<(u16, u16)> {
        match pane {
            // The host names no pane: the live fields are its one grid.
            None => self.last_caret,
            // The live fields are the right grid when the named pane is bound,
            // or when nothing is bound (window-wide state). The second case
            // matters: when a split collapses, `retain_panes` unbinds and the
            // single-pane renderer keeps `last_caret` current in the live
            // fields, while the survivor's parked copy is frozen.
            Some(key) if self.bound.is_none() || self.bound == Some(key) => self.last_caret,
            // Bound to another pane: the named pane's caret is in its parked
            // slot, or nowhere (never scanned), in which case the reach test
            // abstains and the witness refuses on the pane test.
            Some(key) => self.parked.get(&key).and_then(|parked| parked.last_caret),
        }
    }

    /// Whether the live edit keystroke landed within reach of the token at
    /// `row`, `start_col..=end_col`: the place half of the witness, alongside
    /// [`Self::typed_edit_witness`]'s time/pane half. See
    /// [`TYPED_EDIT_REACH_COLS`].
    fn typed_edit_reached(&self, row: u16, start_col: u16, end_col: u16) -> bool {
        let Some(e) = self.typed_edit else {
            return false;
        };
        let Some((edit_row, edit_col)) = e.cell else {
            // No caret has ever been scanned: abstain rather than veto (see
            // [`TypedEdit::cell`]).
            return true;
        };
        edit_row == row
            && edit_col >= start_col.saturating_sub(TYPED_EDIT_REACH_COLS)
            && edit_col
                <= end_col
                    .saturating_add(1)
                    .saturating_add(TYPED_EDIT_REACH_COLS)
    }

    /// Whether a live, unspent edit keystroke witnesses this pane's scan at
    /// `now` (see [`TYPED_EDIT_WITNESS_MS`]): its pane must match
    /// [`Self::scan_scope`], and `None` on either side matches (a host that
    /// records keys with no pane has one grid). Public so hosts can check the
    /// input seam is wired; without it every retype reads as a redraw.
    #[must_use]
    pub fn typed_edit_witness(&self, now: Instant) -> bool {
        self.typed_edit.is_some_and(|e| {
            !e.spent
                && now.saturating_duration_since(e.at)
                    <= Duration::from_millis(TYPED_EDIT_WITNESS_MS)
                && match (e.pane, self.scan_scope()) {
                    (Some(typed_in), Some(scanning)) => typed_in == scanning,
                    _ => true,
                }
        })
    }

    /// Consume the edit witness: this keystroke has now re-armed a cat, and the
    /// repaints that follow it must not ride the same key ([`TypedEdit::spent`]).
    fn spend_typed_edit(&mut self) {
        if let Some(e) = self.typed_edit.as_mut() {
            e.spent = true;
        }
    }

    /// True if the grid changed since the last scan and a rescan is due.
    pub fn needs_rescan(&self, epoch: u64) -> bool {
        !self.have_scanned || epoch != self.last_epoch
    }

    /// Rescan the visible grid into occurrences. Runs only when the damage
    /// epoch advanced; words still present keep their `appeared`, so a word
    /// that merely scrolled does not re-trigger.
    ///
    /// Cold path (introspection capture, tests): resolves each row via
    /// [`Terminal::render_row_into`]. The render path uses
    /// [`rescan_from_cells`](Self::rescan_from_cells) on the snapshot
    /// `cell_frame_into` already built.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan threads the term, grid geometry (rows/cols), lexicon, config, damage epoch, and clock through one call; a wrapper struct would relocate the list, not simplify it"
    )]
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
        // A frozen engine neither scans nor advances, so a capture during a
        // freeze cannot grace-expire episodes against the suspended clock.
        // `last_epoch` is untouched, so the rescan re-fires after thaw.
        if self.frozen_at.is_some() {
            return;
        }
        let opts = cfg.scan_opts();
        self.scan_memo.fit(rows, scan_inputs_fp(lexicon, &opts));
        // A language-gated collision at the editing caret is provisional: the
        // user may still be typing a longer ordinary word (`fut` -> `future`).
        // Scrolled-back content has no live input cursor, so it is settled.
        let cursor = (term.grid().display_offset() == 0).then(|| {
            let cursor = term.cursor();
            (cursor.row, cursor.col)
        });
        // The place half of the typed witness, recorded here because this path
        // hands `rescan_end` a `None` cursor; it must not depend on which
        // rescan entry the host uses ([`Self::last_caret`]).
        if let Some(cell) = cursor {
            self.last_caret = Some(cell);
        }
        let mut out = self.rescan_begin();
        let mut ink_full = false;
        // Take the resident row buffer so `scan_row` can borrow `&mut self`
        // alongside this row's cells; restored below, capacity preserved.
        let mut row_cells = std::mem::take(&mut self.scan_cells);
        let mut start_row = 0usize;
        'pass: loop {
            let pass_start = start_row;
            for r in pass_start..rows {
                term.render_row_into(r, &mut row_cells);
                // §5.7: DEC double-width/height rows suppress the cat (paw + ink
                // fallback); captured per row so the tick needs no terminal access.
                let dec_line = u16::try_from(r)
                    .ok()
                    .and_then(|vr| term.grid().row(vr))
                    .is_some_and(|row| row.line_size() != aterm_core::grid::LineSize::SingleWidth);
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
                        let cutoff = self.dense_scan_cutoff(rows, lexicon, &opts, |rr, buf| {
                            term.render_row_into(rr, buf);
                        });
                        if cutoff > 0 {
                            self.rescan_restart(&mut out, &mut ink_full);
                            start_row = cutoff;
                            continue 'pass;
                        }
                    }
                    break;
                }
            }
            break;
        }
        // The trick flash's rescan half ([`Self::locate_trick_flash`]), after
        // the scan and before `rescan_end`, in both rescan twins.
        self.locate_trick_flash(rows, cols, cursor, now, cfg, &mut row_cells, |r, buf| {
            term.render_row_into(r, buf);
        });
        self.scan_cells = row_cells;
        self.rescan_end(out, cols, epoch, now, None, None);
    }

    /// [`rescan`](Self::rescan) over an already-extracted frame snapshot: the
    /// render path scans the `RenderInput` rows `cell_frame_into` filled under
    /// the same Terminal lock, so the per-cell resolve runs once per frame.
    /// `cells`/`line_sizes` come from the same probes as the term-walking
    /// path, so occurrences, ink capture and the §5.7 `dec_line` flag are
    /// byte-identical (rows arrive in visual order).
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan threads the snapshot rows, grid geometry (rows/cols), lexicon, config, damage epoch, and clock through one call; a wrapper struct would relocate the list, not simplify it"
    )]
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

    /// Snapshot rescan with the live pixel geometry and input caret: cat
    /// palettes sample the exact prospective sprite footprint across adjacent
    /// rows, and language-gated collision forms touching `cursor` stay
    /// provisional until a delimiter moves it away. Pass `None` for
    /// settled/offline content.
    #[allow(
        clippy::too_many_arguments,
        reason = "the live renderer adds one cursor coordinate to the geometry-aware snapshot contract"
    )]
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
        // untouched, so the rescan stays pending).
        if self.frozen_at.is_some() {
            return;
        }
        let opts = cfg.scan_opts();
        self.scan_memo.fit(rows, scan_inputs_fp(lexicon, &opts));
        let mut out = self.rescan_begin();
        let mut ink_full = false;
        let mut start_row = 0usize;
        'pass: loop {
            for (r, row_cells) in cells.iter().enumerate().take(rows).skip(start_row) {
                // §5.7 cat suppression flag, from the snapshot's per-row line sizes
                // (same `Row::line_size` probe the term-walking path makes).
                let dec_line = line_sizes
                    .get(r)
                    .is_some_and(|ls| *ls != aterm_core::grid::LineSize::SingleWidth);
                if !self.scan_row(
                    r,
                    row_cells,
                    cursor,
                    context.map(|_| cells),
                    context.map(|(geom, _)| geom),
                    context.map(|(_, default_bg)| default_bg),
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
                        let cutoff = self.dense_scan_cutoff(rows, lexicon, &opts, |rr, buf| {
                            buf.clear();
                            if let Some(row) = cells.get(rr) {
                                buf.extend_from_slice(row);
                            }
                        });
                        if cutoff > 0 {
                            self.rescan_restart(&mut out, &mut ink_full);
                            start_row = cutoff;
                            continue 'pass;
                        }
                    }
                    break;
                }
            }
            break;
        }
        // The trick flash's rescan half, as in the term-walk twin. This path
        // never touches the resident row buffer, so it is free to lend.
        let mut row_cells = std::mem::take(&mut self.scan_cells);
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

    /// The trick flash's rescan half over either row source (`load_row` fills
    /// `row_buf`). With a flash noted in this scope it finds the typed word on
    /// the caret's row (shifted by the `set_scan_base_y` scroll) or re-verifies
    /// it, and ends the flash when the text is gone, the width changed, or the
    /// pane is scanned without a cursor. It creates no occurrence or episode.
    #[allow(
        clippy::too_many_arguments,
        reason = "threads the rescan's geometry, caret, clock, config and row source to the flash; the dense_scan_cutoff shape"
    )]
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

    /// Shared rescan prologue: reset the §3.6 ordinal counters and the §4.3 ink
    /// capture buffers, and hand back the resident occurrence buffer (cleared).
    fn rescan_begin(&mut self) -> Vec<Occurrence> {
        // §3.6 ordinals are per-rescan (row-major among same-seed occurrences),
        // so the counter map starts fresh; the buffer's capacity is resident.
        self.seed_ordinals.clear();
        // v3 §1.1: a new rescan sequence — episodes adopted this rescan stamp
        // it, and the alignment pass treats everything unstamped as "old".
        self.rescan_seq = self.rescan_seq.wrapping_add(1);
        self.pending.clear();
        self.scan_row_occupied.clear();
        self.pet_ink_spans.clear();
        let mut out = std::mem::take(&mut self.occ);
        out.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        out
    }

    /// Bottom-priority row cutoff for a match-saturated screen: the first row
    /// the restarted scan keeps so everything below fits under
    /// [`MAX_OCCURRENCES`]. Rows are costed bottom-up and whole top rows
    /// dropped, so the prompt keeps its slots; the kept suffix is still scanned
    /// top-down. The estimate overcounts (raw matches), so the restart never
    /// overflows; a single row past the cap is kept and truncates itself.
    /// Every row is visited so `scan_row_occupied` stays truthful.
    fn dense_scan_cutoff(
        &mut self,
        rows: usize,
        lexicon: &Lexicon,
        opts: &ScanOptions<'_>,
        mut load_row: impl FnMut(usize, &mut Vec<RenderCell>),
    ) -> usize {
        let mut row_cells: Vec<RenderCell> = Vec::new();
        if self.scan_row_occupied.len() < rows {
            self.scan_row_occupied.resize(rows, false);
        }
        let mut budget = MAX_OCCURRENCES;
        let mut cutoff = 0usize;
        for r in (0..rows).rev() {
            load_row(r, &mut row_cells);
            self.scan_text.clear();
            for cell in &row_cells {
                if cell.wide {
                    continue;
                }
                self.scan_text.push(cell.ch);
            }
            let occupied = !self.scan_text.trim().is_empty();
            self.scan_row_occupied[r] = occupied;
            if !occupied || cutoff > 0 {
                continue; // above the cutoff only occupancy is recorded
            }
            // The same `(text, opts, lexicon)` scan as `scan_row`, so it shares
            // the memo, sparing a saturated screen a third full tokenise pass.
            // Only the match count is read; the restarted `scan_row` rebuilds
            // its scratch.
            let est = if self.scan_memo.touch(&self.scan_text) {
                self.scan_memo.matches(&self.scan_text).len()
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
        // A single bottom row can estimate past the whole cap; keep it and let
        // the real scan truncate within it.
        cutoff.min(rows.saturating_sub(1))
    }

    /// Clear the per-rescan accumulators for the dense-screen second pass
    /// without advancing `rescan_seq` or touching `scan_row_occupied` (just
    /// rebuilt by [`dense_scan_cutoff`](Self::dense_scan_cutoff)). Episodes the
    /// aborted pass refreshed are on screen, so their stamps are truthful and
    /// replaying under the same sequence is idempotent.
    fn rescan_restart(&mut self, out: &mut Vec<Occurrence>, ink_full: &mut bool) {
        self.seed_ordinals.clear();
        self.pending.clear();
        out.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        *ink_full = false;
    }

    /// Scan one resolved row for lexicon matches, pushing accepted occurrences
    /// into `out` and capturing §4.3 ink from the row's cells.
    ///
    /// `ink_full` is the §4.4 ink truncation latch: once an occurrence's lead
    /// cells do not fit under [`MAX_INK_CELLS`], it and every later occurrence
    /// get no ink, so the settled frame is a stable fixed point.
    ///
    /// Returns `false` when [`MAX_OCCURRENCES`] is reached (the caller stops,
    /// or on the first saturation restarts from `dense_scan_cutoff`).
    #[allow(
        clippy::too_many_arguments,
        reason = "the per-row scanner threads the row cells + index, the §5.7 flag, lexicon, config, clock, and the rescan's occurrence/ink-latch accumulators through one call; a wrapper struct would relocate the list, not simplify it"
    )]
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
        // The pet's ink probe rides the same walk: the row's first and last
        // glyph-bearing columns. A wide continuation is ink when its lead is.
        let mut ink_first = u16::MAX;
        let mut ink_end = 0u16;
        for (c, cell) in row_cells.iter().enumerate() {
            let col = c as u16;
            let inked = if cell.wide {
                ink_end == col
            } else {
                cell.ch != ' ' && !cell.ch.is_whitespace()
            };
            if inked {
                ink_first = ink_first.min(col);
                ink_end = col.saturating_add(1);
            }
            if cell.wide {
                continue; // right half of a wide glyph carries no char
            }
            self.scan_text.push(cell.ch);
            self.scan_colmap.push(c as u16);
        }
        if self.pet_ink_spans.len() <= r {
            self.pet_ink_spans.resize(r + 1, (0, 0));
        }
        self.pet_ink_spans[r] = if ink_end > ink_first {
            (ink_first, ink_end)
        } else {
            (0, 0)
        };
        let occupied = !self.scan_text.trim().is_empty();
        if self.scan_row_occupied.len() <= r {
            self.scan_row_occupied.resize(r + 1, false);
        }
        self.scan_row_occupied[r] = occupied;
        if !occupied {
            return true;
        }
        // Tokenise + lexicon lookup, the bulk of the rescan, served from the
        // [`ScanMemo`] when this row text was scanned recently. The scanner
        // reads only `(text, opts, lexicon)` and the memo retires on either
        // changing, so a hit returns exactly what a scan would.
        let hit = self.scan_memo.touch(&self.scan_text);
        if !hit {
            lexicon.scan_into_with_scratch(
                &self.scan_text,
                opts,
                &mut self.scan_chars,
                &mut self.scan_matches,
                &mut self.scan_scratch,
            );
            self.scan_memo.remember(&self.scan_text, &self.scan_matches);
        }
        // Disjoint field borrows: `matches` pins `scan_memo`/`scan_matches`
        // while the loop below mutates only the accumulators.
        let matches: &[Match] = if hit {
            self.scan_memo.matches(&self.scan_text)
        } else {
            &self.scan_matches
        };
        if hit && !matches.is_empty() {
            // A hit skipped the scanner, so `scan_chars` still holds the
            // previous row's stream, and the SimHash below indexes it with this
            // row's spans. Rebuild it as `scan_into_with_scratch` would; only
            // rows with matches need it.
            self.scan_chars.clear();
            self.scan_chars.extend(self.scan_text.chars());
        }
        // Invariant: whenever this row has matches, `scan_chars` is this row's
        // char stream. `genome::simhash_ctx` indexes it unguarded, so a shorter
        // stale row would panic inside the present. The miss path gets this
        // from the scanner; the hit path from the rebuild above.
        if !matches.is_empty() {
            debug_assert_eq!(
                self.scan_chars.len(),
                self.scan_text.chars().count(),
                "scan_chars must mirror scan_text before the match loop indexes it"
            );
        }
        for &m in matches {
            let start_col = self.scan_colmap.get(m.start).copied().unwrap_or(0);
            let end_lead = self
                .scan_colmap
                .get(m.end.saturating_sub(1))
                .copied()
                .unwrap_or(start_col);
            // Extend to the last on-screen cell: a wide glyph's right half
            // follows its lead column.
            let end_col = if row_cells.get(end_lead as usize + 1).is_some_and(|n| n.wide) {
                end_lead + 1
            } else {
                end_lead
            };
            // Language-gated collision forms touching the live caret are not
            // settled yet (Romanian `fut` while typing `future`). A delimiter
            // moves the caret past `end + 1` and admits the completed form.
            if m.ambiguous
                && cursor.is_some_and(|cell| caret_on_span(cell, r as u16, start_col, end_col))
            {
                continue;
            }
            let live_caret_completion = cursor.is_some_and(|(cursor_row, cursor_col)| {
                cursor_row == r as u16 && cursor_col == end_col.saturating_add(1)
            });
            // The caret is on the token (or the cell after): the typing witness
            // that keeps a retyped kitty from inheriting a spent ancestor.
            let at_live_cursor =
                cursor.is_some_and(|cell| caret_on_span(cell, r as u16, start_col, end_col));
            // A per-word override wins over the class default for any class
            // and bypasses the per-class enable gate (a custom spec on a
            // builtin profanity form fires with `[profanity] enabled = false`).
            let ov = cfg.spec_table.override_for(m.form_hash).copied();
            if ov.is_none() {
                match m.class {
                    Class::Profanity if !cfg.profanity => continue,
                    Class::Feline if !cfg.feline => continue,
                    Class::Canine if !cfg.canine => continue,
                    // `cfg.emphasis` folds `ink_enabled || has_custom_specs`, so
                    // a gated-off emphasis match never takes a slot.
                    Class::Emphasis if !cfg.emphasis => continue,
                    _ => {}
                }
            }
            let (spec, custom) = match ov {
                Some(s) => (s, true),
                None => (class_default_spec(m.class, cfg), false),
            };
            let seed = seed_of(start_col, m.class, m.form_hash);
            // §3.6 identity: ordinal = row-major index among same-seed
            // occurrences this rescan, so twins get distinct episodes.
            let ordinal = {
                let n = self.seed_ordinals.entry(seed).or_insert(0);
                let o = *n;
                *n += 1;
                o
            };
            let ident = mix(seed ^ ordinal.wrapping_mul(ORDINAL_MIX));
            // Fast path: a same-row persist hit adopts the episode and skips
            // SimHash; anything else defers to the rescan-end alignment, with
            // the context fingerprint computed while the row is in scope.
            let seq = self.rescan_seq;
            let ttl = self.grace_override.unwrap_or(GRACE_TTL);
            let mut inert = false;
            let fast = match self.persist.get_mut(&ident) {
                Some(ep)
                    if ep.last_row == r as u16
                        && ep.seed == seed
                        && ep.form_id == m.form_id
                        && !(ep.continuity_tainted
                            && (m.class == Class::Feline
                                || (m.class == Class::Profanity && live_caret_completion)))
                        // Typed-retype guard, as in `alignment_edge`.
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
            let (appeared, genome, cat_colors) = fast.unwrap_or_else(|| {
                // Deferred candidate: the only place the genome is computed.
                // ctx_fp walks ±4 reading-order neighbor tokens (§3.2,
                // allocation-free). If alignment matches a moved episode, the
                // fresh genome is discarded.
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
                    // §3.5: no ident/col/row folded in — the same sentence
                    // rolls the same magic outcome at any indentation.
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
            });
            // Ink capture (§4.3): the base fg of every lead cell in the span,
            // plus the first lead cell's bg for the legibility guard. Wide
            // continuation halves are skipped. Ink-bearing = the resolved spec
            // has an ink axis.
            let ink_bearing = cfg.ink_enabled && spec.ink.is_some();
            let mut ink_base = 0usize;
            let mut ink_cells = 0u16;
            // The first lead cell's bg is captured for every occurrence: the
            // supernova theme branch reads it, and with ink disabled a default
            // black would send light-theme supernovae down the dark path.
            let mut ink_bg = [0u8; 3];
            if let Some(cell) = row_cells[start_col as usize..=end_col as usize]
                .iter()
                .find(|c| !c.wide)
            {
                ink_bg = cell.bg;
            }
            if ink_bearing && !*ink_full {
                let leads = row_cells[start_col as usize..=end_col as usize]
                    .iter()
                    .filter(|c| !c.wide)
                    .count();
                if self.ink_base_fg.len() + leads > MAX_INK_CELLS {
                    *ink_full = true;
                } else {
                    ink_base = self.ink_base_fg.len();
                    ink_cells = leads as u16;
                    for c in start_col..=end_col {
                        let cell = &row_cells[c as usize];
                        if cell.wide {
                            continue;
                        }
                        self.ink_base_fg.push(cell.fg);
                        self.ink_cols.push(c);
                    }
                }
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
                // Resolved through the lexicon instance that produced the
                // match, so the interned id ↔ key pairing cannot skew.
                species: match m.class {
                    Class::Animal => m
                        .species
                        .and_then(|s| animal_glyph_from_key(lexicon.species_code(s))),
                    // A canine word is the animal roster's dog: one species,
                    // two summon paths.
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

    /// Shared rescan epilogue: the row-anchored alignment over deferred
    /// candidates, the §3.6 grace sweep (the done-mark write path), the cat
    /// text-clearance pass, resize-settle bookkeeping, and the epoch/geometry
    /// update.
    fn rescan_end(
        &mut self,
        mut out: Vec<Occurrence>,
        cols: usize,
        epoch: u64,
        now: Instant,
        cat_surface: Option<CatSurface<'_>>,
        // The caret at rescan time, if known; `None` from the cell-less
        // `rescan` path keeps the last observation.
        cursor: Option<(u16, u16)>,
    ) {
        // Resize settle: a cols change opens (or extends) the settle window;
        // births inside it are born-settled and never written to done_marks.
        // It closes on the first rescan at stable cols past the deadline.
        let cols_changed = self.have_scanned && self.cols != cols as u16;
        if cols_changed {
            self.settle_until = Some(now + Duration::from_millis(RESIZE_SETTLE_MS));
        }
        // A returning window spends this rescan's births like a resize settle
        // ([`Self::set_presentable`]). Only the first rescan after return is
        // affected, so typing right after refocus still earns cats.
        let returning = std::mem::take(&mut self.spend_next_births);
        let born_settled = cols_changed || returning || self.settle_until.is_some_and(|t| now < t);
        if !born_settled {
            self.settle_until = None;
        }

        // Expire grace-only episodes before they can bid in alignment. Sequence
        // recency protects an immediate clear/redraw from stalls but must not
        // resurrect an hour-old spent episode.
        let ttl = self.grace_override.unwrap_or(GRACE_TTL);
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

        // Redraw alignment for every deferred candidate: exact seeds keep the
        // vertical-scroll semantics; the context-checked surface fallback
        // carries an episode across a same-width horizontal reflow.
        self.align_pending(&mut out, cols as u16, now, born_settled);

        // Only episodes still unseen after alignment are misses. Nonblank
        // replacement content on their old row means overwrite/typing; blank
        // clear frames keep exact-context grace.
        let rescan_seq = self.rescan_seq;
        for ep in self.persist.values_mut() {
            if ep.seen_seq != rescan_seq
                && self
                    .scan_row_occupied
                    .get(usize::from(ep.last_row))
                    .copied()
                    .unwrap_or(false)
            {
                ep.continuity_tainted = true;
            }
        }

        // Alignment may swap a candidate's provisional genome for the frozen
        // one, so text clearance is recomputed after it to match the art that
        // will render. Only snapshot rescans carry the surface.
        if let Some(surface) = cat_surface {
            for occ in &mut out {
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
        // One cat per caret, pre-companion half: remember the word under the
        // caret now, since `tick` learns the caret only once a companion is on
        // glass, a frame too late ([`Self::caret_word`]). Only a hit
        // overwrites, so walking off the word keeps which word it left.
        if let Some(cell) = cursor
            && let Some(ident) = self
                .occ
                .iter()
                .find(|occ| caret_on_span(cell, occ.row, occ.start_col, occ.end_col))
                .map(|occ| occ.ident)
        {
            self.caret_word = Some(ident);
        }
        // The place half of the typed witness ([`Self::last_caret`]): the caret
        // before the next keystroke's echo. Always overwritten by a
        // cursor-bearing scan (unlike `caret_word`), since a stale caret would
        // vouch for a place the hands have left.
        if let Some(cell) = cursor {
            self.last_caret = Some(cell);
        }
        self.last_epoch = epoch;
        self.have_scanned = true;
        self.cols = cols as u16;
    }

    /// Two-dimensional redraw alignment (v3 §1.1). Deferred candidates group by
    /// exact lexicon surface; an old episode bids when its seed is unchanged
    /// (vertical scroll) or its frozen context matches (horizontal reflow),
    /// seed edges first. Recent leftovers get a bounded monotone match within
    /// two rows, so soft-wrap/status redraws cannot fake births. Matched
    /// episodes move to the candidate's ident; unmatched old ones grace-expire;
    /// unmatched candidates are born.
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
        // The causal half of the re-arm witness ([`Self::note_typed_edit`]),
        // sampled once per rescan: one rescan sees one caret, and
        // `spend_typed_edit` must not change the answer mid-pass.
        let typed_witness = self.typed_edit_witness(now);
        // Set when a re-arm used the keystroke, so it is spent at the tail: one
        // key completes one word.
        let mut typed_witness_used = false;
        let pending = std::mem::take(&mut self.pending);
        // The place half, per candidate: `typed_here[k]` says the key landed
        // within reach of this candidate's token ([`Self::typed_edit_reached`]).
        // Required wherever `typed_witness` is, so a repaint cannot ride an
        // unrelated keystroke.
        let mut typed_here = [false; MAX_OCCURRENCES];
        if typed_witness {
            for (k, p) in pending.iter().enumerate().take(MAX_OCCURRENCES) {
                let occ = &out[usize::from(p.occ_idx)];
                typed_here[k] = self.typed_edit_reached(occ.row, occ.start_col, occ.end_col);
            }
        }
        // Alignment is on the typing path: transient matching state lives in
        // bounded stack scratch (`pending` ≤ MAX_OCCURRENCES, `persist` ≤
        // PERSIST_CAP).
        let mut done = [false; MAX_OCCURRENCES];
        let mut group = [0usize; MAX_OCCURRENCES];
        let mut unseen = [(FormId::UNKNOWN, 0u64); PERSIST_CAP];
        let mut winner: [Option<usize>; MAX_OCCURRENCES] = [None; MAX_OCCURRENCES];
        let mut matched = [false; PERSIST_CAP];
        // The 66 KB decision plane lives in `align_decisions`; borrowed here,
        // restored at the tail. No clearing: every reachable cell is written by
        // the group that reads it.
        let mut decisions = std::mem::take(&mut self.align_decisions);
        if decisions.len() < ALIGN_DECISION_CELLS {
            decisions.resize(ALIGN_DECISION_CELLS, 0);
        }
        let mut prev_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
        let mut curr_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
        // Old-set index, built once: a group's old set is the unseen episodes
        // with its `form_id`, so one `(form_id, key)` pass plus a sort and a
        // binary search per group replaces a full `persist` walk per group.
        // `persist.len() <= PERSIST_CAP` (kept by `insert_persist_bounded`), so
        // the unseen subset fits one `PERSIST_CAP` array.
        let mut unseen_len = 0usize;
        for (&key, ep) in &self.persist {
            if ep.seen_seq != seq {
                debug_assert!(unseen_len < PERSIST_CAP);
                unseen[unseen_len] = (ep.form_id, key);
                unseen_len += 1;
            }
        }
        unseen[..unseen_len].sort_unstable_by_key(|&(f, _)| f);
        for i in 0..pending.len() {
            if done[i] {
                continue;
            }
            let form_id = out[usize::from(pending[i].occ_idx)].form_id;
            // The group's candidates are row-major, then column-major.
            let mut group_len = 0usize;
            for (k, p) in pending.iter().enumerate().skip(i) {
                if !done[k] && out[usize::from(p.occ_idx)].form_id == form_id {
                    done[k] = true;
                    group[group_len] = k;
                    group_len += 1;
                }
            }
            let group = &group[..group_len];
            // Old set: every unseen episode for this exact surface, pulled out
            // of the map (rekeying must never clobber). Strong edges run first;
            // recent leftovers feed the weak continuity arm below.
            self.align_old.clear();
            let lo = unseen[..unseen_len].partition_point(|&(f, _)| f < form_id);
            let hi = unseen[lo..unseen_len].partition_point(|&(f, _)| f == form_id) + lo;
            for &(_, k) in &unseen[lo..hi] {
                // Earlier groups may have mutated the map (a rekey collision
                // replacing an occupant, an eviction), so the index is only a
                // candidate list: re-check the exact membership predicate.
                // Order does not matter; `align_old` is sorted below by
                // `(last_row, last_col, key)`.
                if self
                    .persist
                    .get(&k)
                    .is_some_and(|ep| ep.form_id == form_id && ep.seen_seq != seq)
                    && let Some(ep) = self.persist.remove(&k)
                {
                    self.align_old.push((k, ep));
                }
            }
            self.align_old
                .sort_unstable_by_key(|(k, ep)| (ep.last_row, ep.last_col, *k));

            // One monotone alignment DP over all three evidence arms: stationary
            // survivors first (the log-rotation discriminator), then maximum
            // cardinality (no births manufactured by greedy ties), then seed >
            // context > recent continuity, then reading-order distance.
            // Monotonicity keeps twins from crossing.
            winner[..group_len].fill(None);
            prev_scores[..=group_len].fill(AlignScore::default());
            decisions[1..=group_len].fill(2); // row zero: skip candidate
            let same_width = self.have_scanned && self.cols == cols;
            for oi in 1..=self.align_old.len() {
                curr_scores[0] = prev_scores[0];
                decisions[oi * (MAX_OCCURRENCES + 1)] = 1; // skip old
                let ep = &self.align_old[oi - 1].1;
                for gj in 1..=group_len {
                    let mut best = prev_scores[gj];
                    let mut decision = 1u8; // skip old
                    if curr_scores[gj - 1].better_than(best) {
                        best = curr_scores[gj - 1];
                        decision = 2; // skip candidate
                    }
                    let k = group[gj - 1];
                    let candidate = pending[k];
                    let occ = &out[usize::from(candidate.occ_idx)];
                    if let Some((tier, distance)) =
                        alignment_edge(ep, candidate, occ, same_width, seq, cols)
                    {
                        // The group already proves surface equality; position
                        // defines the survivor, so case-only redraws still pin.
                        let stationary = ep.last_row == occ.row && ep.last_col == occ.start_col;
                        let score = prev_scores[gj - 1].with_edge(stationary, tier, distance);
                        if score.better_than(best) || score == best {
                            best = score;
                            decision = 3; // match
                        }
                    }
                    curr_scores[gj] = best;
                    decisions[oi * (MAX_OCCURRENCES + 1) + gj] = decision;
                }
                std::mem::swap(&mut prev_scores, &mut curr_scores);
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
            for &oi in winner[..group_len].iter().flatten() {
                matched[oi] = true;
            }
            let group_class = out[usize::from(pending[group[0]].occ_idx)].class;
            let has_tainted_old = self.align_old.iter().any(|(_, ep)| ep.continuity_tainted);
            // Typed = a committed keystroke within reach of this token (causal,
            // and about this word) and the caret at the token's end.
            let group_has_live_caret_completion = group
                .iter()
                .any(|&k| typed_here[k] && pending[k].live_caret_completion);
            // Neither taint (any screen change sets it) nor caret position (a
            // shell parks the caret on the line it drew) proves a human typed,
            // so both classes also require the keystroke witness.
            //
            // The keys a typed candidate in this group will be born under
            // (≤ MAX_OCCURRENCES). The unmatched-old sweep below deletes a spent
            // ancestor's guard only for these keys, so retyping one occurrence
            // does not unprotect another.
            let mut typed_keys = [0u64; MAX_OCCURRENCES];
            let mut typed_keys_len = 0usize;
            for &k in group.iter() {
                if typed_here[k] && pending[k].at_live_cursor && typed_keys_len < MAX_OCCURRENCES {
                    let occ = &out[usize::from(pending[k].occ_idx)];
                    typed_keys[typed_keys_len] = occ.ident ^ pending[k].ctx_fp;
                    typed_keys_len += 1;
                }
            }
            let group_has_at_live_cursor = typed_keys_len > 0;

            // Apply: matched moves + fresh births first, then unmatched
            // reinsertion (flush on key collision).
            for (j, &k) in group.iter().enumerate() {
                let occ = &mut out[usize::from(pending[k].occ_idx)];
                if let Some(oi) = winner[j] {
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
                    // A transferred episode may land on a new `(ident, ctx_fp)`;
                    // its stamped mark belongs to the old key, so the latch
                    // reopens and the next tick stamps the new one.
                    ep.ledger_stamped = false;
                    occ.appeared = ep.appeared;
                    occ.genome = ep.genome;
                    occ.cat_colors = ep.cat_colors;
                    occ.inert = ep.inert();
                    self.rekey_persist_episode(old_ident, occ.ident, ep);
                } else {
                    // Fresh episode: done_marks lookup is free here — ctx_fp
                    // was already computed on the (deferred) miss path.
                    let key = occ.ident ^ pending[k].ctx_fp;
                    // Typed re-entry: `typed_here[k]` (a committed keystroke
                    // within reach of this token, which no PTY output can
                    // synthesize) plus the class's caret witness. It re-arms:
                    // neither a prior done mark nor the old episode's departure
                    // makes it inert, and a feline's colliding done mark is
                    // deleted. Resident old evidence is not required, since a
                    // done mark outlives its episode's `GRACE_TTL`; a passive
                    // re-appearance has no keystroke and does not re-arm.
                    let typed_reentry = typed_here[k]
                        && ((group_class == Class::Feline && pending[k].at_live_cursor)
                            || (group_class == Class::Profanity
                                && has_tainted_old
                                && pending[k].live_caret_completion));
                    let born_done = if typed_reentry {
                        // The keystroke is now spent on a cat, so repaints
                        // inside the witness window cannot re-arm again.
                        typed_witness_used = true;
                        self.done_marks.remove(&key);
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
                    // `birth_seq` advances on every persist miss; the burst roll
                    // is decided once here, per appearance (repeats decorrelate),
                    // and stored so alignment transfers it.
                    self.birth_seq = self.birth_seq.wrapping_add(1);
                    if !(born_done || born_settled)
                        && let Some(b) = occ.spec.burst
                        && b.chance_pct > 0
                    {
                        // §3.2b f-bomb combo: a typed f-bomb charges the streak
                        // before the decode, so this birth counts itself; other
                        // births ride the ambient level. Classic novas and
                        // custom bursts neither charge nor escalate, and chance
                        // 0 stays an absolute off-switch.
                        let level = if b.kind == BurstKind::SuperNova {
                            let typed = typed_here[k]
                                && group_class == Class::Profanity
                                && pending[k].live_caret_completion;
                            self.combo_level(now, typed)
                        } else {
                            0
                        };
                        // One draw, two decodes: `mix` is the splitmix64
                        // finalizer, so its halves are independent. The low half
                        // decides whether to detonate, the high half which of
                        // the three degrees; the combo level picks which window
                        // table reads each half.
                        let draw =
                            mix(occ.genome.gkey ^ self.birth_seq ^ supernova::SUPERNOVA_SALT);
                        ep.burst_roll =
                            draw % 100 < u64::from(supernova::combo_chance(b.chance_pct, level));
                        ep.burst_tier = supernova::tier_for(draw, level);
                    }
                    // Curse bonk typed witness, latched at the only fresh-birth
                    // site: caret one past the token and a committed keystroke
                    // within reach (output leaves the caret one past a curse
                    // too, and this makes a sound). Inert births stay silent.
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
                        // Inert on every time-windowed arm: the peek, the
                        // burst, and the sweep are all pre-spent.
                        ep.peek_done = true;
                        ep.burst_done = born_done;
                        ep.sweep_done = true;
                        ep.nova_done = true;
                        occ.inert = true;
                    }
                    self.insert_persist_bounded(occ.ident, ep, PersistAdmission::Observed);
                }
            }
            for (oi, is_matched) in matched[..self.align_old.len()].iter().copied().enumerate() {
                if is_matched {
                    continue;
                }
                let (old_ident, ep) = self.align_old[oi];
                if ep.continuity_tainted
                    && ((group_class == Class::Feline && group_has_at_live_cursor)
                        || (group_class == Class::Profanity && group_has_live_caret_completion))
                {
                    // Typed re-entry closes the old episode as an intentional
                    // retype and must not leave a done mark that makes the new
                    // token inert. Same typed witness as the birth site above,
                    // else a repaint would delete the mark and re-arm the cat.
                    // Key-matched, not group-wide: retyping one `kitty` must not
                    // strip another's guard.
                    let old_key = old_ident ^ ep.ctx_fp;
                    if group_class == Class::Feline
                        && !typed_keys[..typed_keys_len].contains(&old_key)
                    {
                        self.insert_persist_bounded(old_ident, ep, PersistAdmission::Grace);
                        continue;
                    }
                    self.done_marks.remove(&old_key);
                    continue;
                }
                // Key free and capacity available: keep it to grace-expire. A
                // claimed key or a map refilled by visible candidates makes it
                // depart instead.
                self.insert_persist_bounded(old_ident, ep, PersistAdmission::Grace);
            }
        }
        self.pending = pending;
        self.align_decisions = decisions;
        self.align_old.clear();
        // One keystroke, at most one re-arm. Spent at the tail so the pass
        // reads one consistent witness; from the next rescan on, repaints see
        // it spent.
        if typed_witness_used {
            self.spend_typed_edit();
        }
    }

    /// Advance the animation one frame and emit into `out` (decorations), `ink`
    /// (fg overrides, sorted unique `(row, col)`: row-major emission, the trick
    /// flash merged in order), `free` (one [`FreeSprite`] per cat, paired with
    /// [`WordDecorations::free_atlas`]) and `nova` (§6 premultiplied row-band
    /// GlowQuads for `nova_add`).
    ///
    /// `companion` is the cursor companion drawn into this `free` stream this
    /// frame, if any: its `cell` drives the one-cat-per-caret claim and
    /// caret-on-span gate, its `body_px` the pixel yield. `sel` is the
    /// selection view (§6.4); `_focused` is unused (hosts demote unfocused
    /// windows to `reduced_motion`).
    ///
    /// Returns a fingerprint that changes every frame while anything animates
    /// and is stable otherwise; empty outputs with fp `0` mean nothing to draw.
    #[allow(
        clippy::too_many_arguments,
        reason = "the per-frame tick threads the clock, config, geometry, companion view, selection view, focus gate, and four output scratches through one call; a wrapper struct would relocate the list, not simplify it"
    )]
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
        // One cat per caret: the companion's cell feeds both the claim below
        // and the `caret_on_span` gate, so the word it answers never grows an
        // ambient twin.
        let companion_at = companion.and_then(|c| c.cell);
        out.clear();
        ink.clear();
        free.clear();
        nova.clear();
        // Cleared at tick start (not drain time) so undrained hostless builds
        // stay bounded at MAX_OCCURRENCES.
        self.sightings.clear();
        // Curse cues likewise (bounded at MAX_CURSE_CUES); typed one-shots live
        // on the episode latch, so an undrained frame loses only sound.
        self.curse_cues.clear();
        // Peek cues too (bounded at MAX_PEEK_CUES): an undrained frame loses
        // only one bat gag, never correctness.
        self.peek_cues.clear();
        if !self.host_brackets_frames {
            // No host frame bracket: one baker prologue per tick. A bracketing
            // host resets it once per presented frame, so panes share the budget.
            self.cat_baker_ready = false;
        }
        // One cat per caret, part 1: maintain the companion claim
        // ([`Self::companion_claim`]) before anything emits and before the early
        // returns below, so a companion leaving over an empty or frozen grid
        // still releases it. While a companion is up the claim follows the
        // caret to each feline word. Bounded: `self.occ` ≤ MAX_OCCURRENCES.
        match (companion, companion_at) {
            // No animal on glass: the episode is over, the claim releases.
            (None, _) => self.companion_claim = None,
            // A resident body is still fading but DECTCEM gave no caret cell:
            // keep the last claim and let the `body_px` collision own this
            // frame.
            (Some(_), None) => {}
            // Companion on glass: resolve the word it answers now. The companion
            // arrives a frame after the tick that scanned its word, by which
            // time the caret may be past `end_col + 1`, so fall back to
            // [`Self::caret_word`] — only while that ident is still on screen.
            (_, Some(cell)) => {
                let live = self
                    .occ
                    .iter()
                    .find(|occ| caret_on_span(cell, occ.row, occ.start_col, occ.end_col))
                    .map(|occ| occ.ident)
                    .or_else(|| {
                        self.caret_word
                            .filter(|id| self.occ.iter().any(|occ| occ.ident == *id))
                    });
                // A hit re-targets the claim (the caret reached a new feline
                // word); a miss keeps it, so the veto outlasts the caret's visit
                // while the companion is flying.
                if live.is_some() {
                    self.companion_claim = live;
                }
            }
        }
        // A frozen engine emits nothing and advances nothing (defensive: hosts
        // skip the tick while suspended; `thaw` restores).
        if self.frozen_at.is_some() {
            self.novas.clear();
            self.coupling.clear();
            return 0;
        }
        if self.occ.is_empty() {
            // Trick flash on a wordless screen (`sit` at a clean prompt): this
            // arm returns before `frame` advances, so the flash arms its own
            // deadline and folds its own clock, or it would freeze mid-ramp.
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
                    ink.windows(2)
                        .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
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
        // Episode prepass: arm freezing (Cat vs Paw stored at the first
        // emission decision) and the per-axis one-shot flags (peek/burst/sweep
        // started/done), the done-mark write condition. The peek clock is
        // latched at birth ([`Episode::fresh`]).
        self.episode_prepass(now, cfg, geom);
        let anim = Duration::from_millis(cfg.anim_ms);
        let mut fp = FNV64_OFFSET;
        let mut active_until: Option<Instant> = None;
        // The nuke cloud's `(t_ms, cx, cy)`, recorded in the occurrence loop and
        // emitted after it (baking needs `self.cat_baker`, which the loop holds).
        // `MAX_ACTIVE_SUPERNOVAE = 1`, so one slot suffices.
        let mut nuke_pending: Option<(u64, i32, i32)> = None;
        // Supernova prepass first: it owns the burst mutex (`super_until`) the
        // classic prepass defers behind, and defers behind a live classic
        // window itself. The windows never overlap, so this pass's `nova_add`
        // share stays within `max(3·392, S_max) ≤ MAX_NOVA_QUADS`; prism wake
        // appends outside it.
        if let Some(until) = self.super_prepass(now, cfg, geom, sel) {
            arm_until(&mut active_until, until);
        }
        // §6.4 nova prepass: limiter grants, window expiry, and this frame's
        // live-nova and coupling scratch, before the emission loop so ink
        // coupling (§6.5) sees every live nova. Keeps the scheduler armed
        // through delayed ignitions.
        if let Some(until) = self.nova_prepass(now, cfg, geom, sel) {
            arm_until(&mut active_until, until);
        }
        // Curse bonk typed cues: turn birth-latched one-shots into this tick's
        // cues, after the prepasses (so detonation cues share the drain) and
        // before the emission loop (which holds `persist`). The latch clears
        // only on a successful queue.
        for occ in &self.occ {
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
                break; // cap-truncated: the latch survives and retries
            }
            ep.bonk_pending = false;
            self.curse_cues.push(CurseCue {
                kind: CurseCueKind::Typed,
                row: occ.row,
                col: occ.start_col,
            });
        }
        // Disjoint field borrows: the cat arm bakes and commits (&mut baker)
        // while the loop reads occurrences, ink capture and nova scratch.
        let frame = self.frame;
        // Read out before the loop takes its field borrows (the graphic arm's
        // one-cat-per-caret gate reads it per occurrence).
        let claim = self.companion_claim;
        // Unlogged-episode scratch (§F4.2), from a shared `&persist` borrow; the
        // logged commit waits for the post-loop pass. Inert episodes never log.
        self.unlogged.clear();
        for occ in &self.occ {
            if occ.spec.graphic.is_some()
                && !occ.inert
                && self.persist.get(&occ.ident).is_some_and(|ep| !ep.logged)
            {
                self.unlogged.push(occ.ident);
            }
        }
        let mut ctx = CatTick {
            baker: &mut self.cat_baker,
            animal_baker: &mut self.animal_baker,
            unlogged: &self.unlogged,
            sightings: &mut self.sightings,
            peek_cues: &mut self.peek_cues,
            // The rect the host is about to draw; neither animal sits on the
            // caret cell, so a caret band alone would under-cover it.
            companion_px: companion.map_or_else(CompanionFootprint::default, |c| c.footprint(geom)),
        };
        let mut cats = 0usize;
        let mut animals = 0usize;

        for (oi, occ) in self.occ.iter().enumerate() {
            // The occurrence's episode, looked up once. Only the graphic and
            // burst axes (and the nova's ink fx) read it, so an ink-only word
            // skips it. Direct-built occurrences (tests/demos) have none.
            let ep = if occ.spec.graphic.is_some() || occ.spec.burst.is_some() {
                self.persist.get(&occ.ident)
            } else {
                None
            };
            // §6 ink modifiers: the word's own nova (palette anchors, Dip,
            // sweep freeze, ember tint) + the §6.5 blast-coupling pulse.
            let fx = ink_fx(
                oi,
                occ,
                cfg,
                now,
                geom,
                &self.novas,
                &self.supers,
                &self.coupling,
                ep,
            );
            // Ink is emitted independently of the deco cap: its own budget was
            // enforced at rescan (≤ MAX_INK_CELLS captured lead cells).
            emit_ink(
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
            // v3 §6 graphic axis (`Collection::Cats`: the peeking cat, with
            // its documented paw fallbacks). Ink-only specs skip it entirely.
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
                    // Arm freezing: dispatch from the stored arm when an episode
                    // has one, so rank churn past MAX_CATS, DECDHL flips or floor
                    // crossings never morph a dwelling cat into a paw or back.
                    // Direct-built occurrences resolve the arm per frame.
                    let peek = PeekView {
                        start: ep.map_or(Some(occ.appeared), |e| e.phase_start),
                        peek_done: ep.is_some_and(|e| e.peek_done),
                        arm: ep.and_then(|e| e.shown_as),
                    };
                    // Done is unconditional under reduced motion: native hosts
                    // demote unfocused windows to reduced_motion, which must not
                    // resurrect a completed one-shot.
                    if peek.peek_done {
                        break 'graphic; // Done: zero quads forever (duty pin)
                    }
                    // One cat per caret, part 2: the companion the host draws is
                    // this word's cat, so the word draws none. It is that word
                    // by the claim (which holds while the companion flies) or by
                    // the caret on the span. Graphic-only: ink still plays, and
                    // the arm census and Kitty Log are untouched.
                    let vetoed_by = |cell: Option<(u16, u16)>| {
                        cell.is_some_and(|c| caret_on_span(c, occ.row, occ.start_col, occ.end_col))
                    };
                    if (companion_at.is_some() && claim == Some(occ.ident))
                        || vetoed_by(companion_at)
                    {
                        break 'graphic;
                    }
                    // Animal roster: the feline's one-shot arm law (frozen arms,
                    // overflow draws nothing, ink always survives), with its own
                    // MAX_ANIMALS pool, no feline_style gate and no paw.
                    if occ.spec.graphic.map(|g| g.collection) == Some(Collection::Animals) {
                        let Some(species) = occ.species else {
                            break 'graphic; // tag without shipped art: ink only
                        };
                        let eligible = peek_eligible(occ, geom);
                        let arm = peek.arm.unwrap_or(if !eligible {
                            KittyShownAs::PawFallbackFloor
                        } else if animals < MAX_ANIMALS {
                            KittyShownAs::Cat
                        } else {
                            KittyShownAs::PawFallbackOverflow
                        });
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
                    let arm = peek.arm.unwrap_or_else(|| {
                        if cfg.feline_style != FelineStyle::Cat {
                            KittyShownAs::PawStyle
                        } else if !eligible {
                            KittyShownAs::PawFallbackFloor
                        } else if cats < MAX_CATS {
                            KittyShownAs::Cat
                        } else {
                            KittyShownAs::PawFallbackOverflow
                        }
                    });
                    // The only feline graphic is the peeking cat; every fallback
                    // arm (style, cell floors, narrow, top row, MAX_CATS
                    // overflow) draws nothing, and the ink still plays.
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
                        // A stored Cat that cannot draw emits nothing while
                        // its peek clock is paused (`peek_pause`).
                    }
                }
            }
            // v3 §6 burst axis.
            match occ.spec.burst.map(|b| b.kind) {
                None => {}
                // §6 classic nova — see `emit_nova_axis`.
                Some(BurstKind::Nova) => {
                    emit_nova_axis(
                        occ,
                        oi,
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
                // v3 §3.2 supernova — see `emit_super_axis`.
                Some(BurstKind::SuperNova) => {
                    emit_super_axis(
                        occ,
                        oi,
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
                // Soft additive glow pulse (custom `kind = "glow"`): one
                // rise-and-fall over ~1.4 s, then done (§1.2).
                Some(BurstKind::Glow) => {
                    if occ.inert || cfg.reduced_motion {
                        continue;
                    }
                    // v3 §6 `chance_pct`: a rolled-off glow never fires.
                    if ep.is_some_and(|e| !e.burst_roll) {
                        continue;
                    }
                    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
                    if t < GLOW_BURST_MS && out.len() < MAX_DECORATIONS {
                        let until = occ.appeared + Duration::from_millis(GLOW_BURST_MS);
                        arm_until(&mut active_until, until);
                        let e = (core::f32::consts::PI * t as f32 / GLOW_BURST_MS as f32)
                            .sin()
                            .max(0.0);
                        let mid = occ.start_col + (occ.end_col - occ.start_col) / 2;
                        let d = WordDecoration {
                            row: occ.row,
                            col: mid,
                            dx: 0,
                            dy: -((i32::from(geom.cell_h) / 3).min(127) as i8),
                            glyph: DecoGlyph::Star4,
                            blend: DecoBlend::Add,
                            // The one gold.
                            color: crate::effect_util::twinkle_rgb(true),
                            alpha: scale_u8(cfg.intensity * 0.8 * e),
                        };
                        fp = fold_deco(fp, &d) ^ frame.wrapping_mul(0x9E37_79B1);
                        out.push(d);
                    }
                }
                Some(BurstKind::Sparkle | BurstKind::Starburst) => {
                    // v3 §1.1: born-done / born-settled sparkles are inert —
                    // the burst played (or resize-settled); zero decos.
                    if occ.inert {
                        continue;
                    }
                    // `chance_pct`: a rolled-off burst never fires. Direct-built
                    // occurrences have no episode and always fire.
                    if ep.is_some_and(|e| !e.burst_roll) {
                        continue;
                    }
                    let age_s = now.saturating_duration_since(occ.appeared).as_secs_f32();
                    let animating =
                        !cfg.reduced_motion && now.saturating_duration_since(occ.appeared) < anim;
                    let width = u64::from(occ.end_col.saturating_sub(occ.start_col) + 1).max(1);
                    if animating {
                        let until = occ.appeared + anim;
                        arm_until(&mut active_until, until);
                        let density = cfg.density.clamp(1, 12);
                        for k in 0..density {
                            if out.len() >= MAX_DECORATIONS {
                                break;
                            }
                            let s = mix(occ.seed
                                ^ u64::from(k).wrapping_mul(0xA24B_AED4_963E_E407)
                                ^ self.frame.rotate_left(17));
                            let col = occ.start_col + (s % width) as u16;
                            let glyph = cfg
                                .glyphs
                                .get((s >> 11) as usize % cfg.glyphs.len().max(1))
                                .copied()
                                .unwrap_or(DecoGlyph::Star4);
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
                            // Mixing the frame in keeps the fp changing each animating
                            // frame so the present early-out never skips a live sparkle.
                            fp = fold_deco(fp, &d) ^ self.frame.wrapping_mul(0x9E37_79B1);
                            out.push(d);
                        }
                    } else {
                        // Graphics decay (§1.2): the residual fades out within
                        // 2 s after the animation window, then nothing; the
                        // reduced-motion static spark keeps its fixed bytes.
                        let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
                        let f = if cfg.reduced_motion {
                            // Under reduced motion: no static resurrection once
                            // the burst one-shot faded (`burst_done`). A
                            // reduced-from-birth episode never advances the
                            // flag, so its static spark persists.
                            (!ep.is_some_and(|e| e.burst_done)).then_some(1.0f32)
                        } else if t < cfg.anim_ms + RESIDUAL_FADE_MS {
                            let until = occ.appeared
                                + Duration::from_millis(cfg.anim_ms + RESIDUAL_FADE_MS);
                            arm_until(&mut active_until, until);
                            Some(1.0 - (t - cfg.anim_ms) as f32 / RESIDUAL_FADE_MS as f32)
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
        // The nuke cloud (`nuke_pending`): base surge, column and head tiles,
        // each placed by `nuke_draw`, drawn back to front, then the cap tile
        // again as the smaller, hotter core. Art, so the FreeSprite lane.
        if let Some((t, cx, cy)) = nuke_pending {
            for (slot, part, core) in [
                (0usize, crate::nuke::NukePart::Skirt, false),
                (1, crate::nuke::NukePart::Stem, false),
                (2, crate::nuke::NukePart::Cap, false),
                (2, crate::nuke::NukePart::Cap, true),
            ] {
                let d = if core {
                    crate::nuke::nuke_core_draw(t)
                } else {
                    crate::nuke::nuke_draw(t, part)
                };
                let Some(d) = d else {
                    continue;
                };
                let (nw, nh) = crate::nuke::nuke_nat_size(part, geom.cell_w, geom.cell_h);
                let host = crate::nuke::nuke_host_id(part, nw, nh);
                let Some(tile) = NotePaintCache::tile(
                    &mut self.nuke_cache[slot],
                    &mut self.cat_baker,
                    host,
                    (nw, nh),
                    || crate::nuke::bake_nuke(nw, nh, part),
                ) else {
                    continue; // bake budget spent; this part lands next frame
                };
                let dest_w = ((f32::from(nw) * d.sx).round() as i32).max(1);
                let dest_h = ((f32::from(nh) * d.sy).round() as i32).max(1);
                let grid_w = i32::from(geom.cols) * i32::from(geom.cell_w);
                if dest_w > grid_w {
                    continue; // a grid narrower than the cloud stays clear
                }
                // Anchored on the blast, standing on the word's row and
                // growing upward from it.
                let base_y = cy + i32::from(geom.cell_h) / 2;
                let dy = (d.dy_cells * f32::from(geom.cell_h)).round() as i32;
                let alpha = (d.alpha * f32::from(u8::MAX)) as u8;
                if alpha == 0 {
                    continue;
                }
                // At most four sprites, one cloud at a time
                // (`MAX_ACTIVE_SUPERNOVAE = 1`): constant cost.
                free.push(FreeSprite {
                    x: (cx - dest_w / 2).clamp(0, (grid_w - dest_w).max(0)),
                    y: base_y - dest_h + dy,
                    w: dest_w as u16,
                    h: dest_h as u16,
                    ax: tile.ax,
                    ay: tile.ay,
                    aw: nw,
                    ah: nh,
                    tint: d.tint,
                    alpha,
                    flip_x: false,
                    z: FreeZ::OverText,
                    sampler: FreeSampler::Nearest,
                });
            }
        }
        // Mark exactly the queued sightings logged (§F4.2); a cap-truncated or
        // never-emitted cat stays unlogged and retries next tick.
        for s in &self.sightings {
            if let Some(ep) = self.persist.get_mut(&s.ident) {
                ep.logged = true;
            }
        }
        // A rebake must repaint, so the atlas version joins the fingerprint
        // whenever cat sprites are on screen.
        if !free.is_empty() {
            fp = fnv_mix_u64(fp, self.cat_baker.version());
        }
        // The trick flash, merged last so it sees what the occurrences inked: at
        // most 32 cells in `(row, col)` order; where an occurrence already inked
        // a cell, that colour is the mix base, so the fade lands on what shows
        // without the flash. It folds into `fp` and arms the deadline only
        // while it emits.
        let scope = self.scan_scope();
        if let Some(until) =
            self.trick_flash
                .emit(scope, now, trick_flash_paintable(cfg), ink, &mut fp)
        {
            arm_until(&mut active_until, until);
        }
        // The renderers' merge-walk needs sorted-unique (row, col), which is
        // structural here; checked in debug builds only.
        debug_assert!(
            ink.windows(2)
                .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
            "ink must be sorted by (row, col) with unique cells"
        );
        self.active_until = active_until;
        fp
    }

    /// Episode prepass, before emission each tick:
    ///
    /// * **Arm freezing**: a graphic episode's Cat-vs-Paw arm is stored at the
    ///   first emission decision (`MAX_CATS` counts visible, non-done stored
    ///   Cats). A stored fallback drew nothing, so it is re-decided each tick
    ///   and may upgrade to Cat once; a stored Cat is never downgraded.
    /// * **Phase clock**: latched at birth; it pauses (`peek_pause`) while a
    ///   stored Cat cannot draw and shifts forward on resume.
    /// * **One-shot flags** per axis (peek, burst, ink sweep): the
    ///   `done_marks` write condition.
    ///
    /// Inert episodes and direct-built occurrences are skipped.
    fn episode_prepass(&mut self, now: Instant, cfg: &DecoConfig, geom: EffectGeom) {
        // §5.2 slot census over the visible occurrences only: grace-only
        // episodes cannot emit, and counting them would let departed history
        // starve every slot. Cats and animals share the `KittyShownAs::Cat`
        // encoding but count into separate pools (MAX_CATS vs MAX_ANIMALS).
        let (mut cat_slots, mut animal_slots) =
            self.occ.iter().fold((0usize, 0usize), |(c, a), occ| {
                let live = self.persist.get(&occ.ident).is_some_and(|e| {
                    e.shown_as == Some(KittyShownAs::Cat) && !e.peek_done && !e.inert()
                });
                match occ.spec.graphic.map(|g| g.collection) {
                    _ if !live => (c, a),
                    Some(Collection::Animals) => (c, a + 1),
                    _ => (c + 1, a),
                }
            });
        for occ in &self.occ {
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                continue;
            };
            if ep.inert() {
                continue;
            }
            // Ink-sweep axis: captured ink means the sweep began at `appeared`;
            // it settles after its colorway window. A looping sweep never
            // finishes.
            if cfg.ink_enabled && occ.ink_cells > 0 && occ.spec.ink.is_some() {
                ep.sweep_started = true;
                let t = now.saturating_duration_since(ep.appeared).as_millis() as u64;
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
            // v3 §6 graphic axis (custom cat words included).
            if let Some(g) = occ.spec.graphic {
                {
                    // Live drawability (cell floors, DEC lines, word width, the
                    // two-row text clearance recomputed every rescan). The arm
                    // decision, fallback re-evaluation and clearance pause use the
                    // emission loop's predicate, so an armed Cat could draw at
                    // decision time.
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
                        // Animals reuse the arm encoding (`Cat` = the authored
                        // sprite) with their own slot pool and no feline_style
                        // gate.
                        let arm = if !is_animal && cfg.feline_style != FelineStyle::Cat {
                            KittyShownAs::PawStyle
                        } else if !drawable {
                            // Drawability includes text clearance, so a word born
                            // under dense output does not claim a §5.2 slot.
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
                            // Upgrading a fallback that showed nothing: re-latch
                            // the phase clock so the entrance plays in full.
                            ep.phase_start = Some(now);
                            ep.peek_pause = None;
                        }
                        ep.shown_as = Some(arm);
                    }
                    // The phase clock latched at birth. Under reduced_motion the
                    // static pose is the showing and the flags never advance, so
                    // the pose stays.
                    match ep.shown_as {
                        // A spent peek is skipped whole: nothing in this arm can
                        // transition once `peek_done` is latched, and every
                        // reader of `peek_pause`, `phase_start` and `peek_total`
                        // is gated on `!peek_done`. Saves re-deriving the dwell
                        // chain every frame for finished cats.
                        Some(KittyShownAs::Cat) if !ep.peek_done => {
                            // Clearance pause/resume: text flooding the body
                            // footprint hides the cat, but the one-shot waits.
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
                                // Must mirror emit_cat's dwell derivation (magic,
                                // accessory, spec dwell range); animals mirror
                                // emit_animal's (magical = false). Latched so the
                                // scrolled-off sweep can finish the one-shot.
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
                        // Re-evaluable fallback arms (floor/overflow) drew
                        // nothing and must not insta-complete: that would make a
                        // slot-starved typed word inert and its departure mark
                        // would poison future retypes.
                        _ => {}
                    }
                }
            }
            // Burst axis flags. Nova/SuperNova flags belong to their prepasses
            // (ignition grant is the point of no return). A rolled-off burst
            // never fires, so its flags never advance.
            match occ.spec.burst.map(|b| b.kind) {
                Some(BurstKind::Sparkle | BurstKind::Starburst)
                    if !cfg.reduced_motion && ep.burst_roll =>
                {
                    ep.burst_started = true;
                    let t = now.saturating_duration_since(ep.appeared).as_millis() as u64;
                    if t >= cfg.anim_ms + RESIDUAL_FADE_MS {
                        ep.burst_done = true;
                    }
                }
                Some(BurstKind::Glow) if !cfg.reduced_motion && ep.burst_roll => {
                    ep.burst_started = true;
                    let t = now.saturating_duration_since(ep.appeared).as_millis() as u64;
                    if t >= GLOW_BURST_MS {
                        ep.burst_done = true;
                    }
                }
                _ => {}
            }
        }
        // Scrolled-off mid-peek completion and the in-place ledger stamp share
        // one walk of `persist` ([`Self::stamp_spent_marks`]).
        self.stamp_spent_marks(now, cfg.reduced_motion);
    }

    /// Stamp the done mark of every started one-shot episode while it is still
    /// resident, and finish scrolled-off mid-peek Cat episodes.
    ///
    /// Departure paths (grace expiry, `reset_bound`, eviction) write marks only
    /// when an episode leaves the persist map. An episode replaced in place by
    /// a redraw never leaves, so without this stamp the newcomer's
    /// `touch_done` misses and replays the cat on every repaint. The write
    /// condition is departure's ([`Episode::one_shot_started`]), so a repaint
    /// during the rise or dwell also finds a mark; [`Episode::ledger_stamped`]
    /// limits it to one LRU touch per episode per identity. A genuine retype
    /// still re-arms: `typed_reentry` deletes the mark first.
    ///
    /// Reduced motion advances no axis, so it stamps only flags already set
    /// (born-done births set theirs regardless): an armed replacement shows the
    /// same static pose, where a born-done one would show no cat. A
    /// horizontally reflowed episode may also leave a mark at its pre-reflow
    /// key; that errs toward fewer cats, and deleting the old key could drop
    /// an older episode's guard.
    fn stamp_spent_marks(&mut self, now: Instant, reduced_motion: bool) {
        let seq = self.rescan_seq;
        // Disjoint field borrows: the ledger is written from a walk of the
        // episode map, and `done_marks` is a sibling field.
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
            // The in-place ledger stamp proper (see the doc above).
            if !ep.ledger_stamped && ep.one_shot_started() {
                mark_done(marks, *ident, ep);
                ep.ledger_stamped = true;
            }
        }
    }

    /// Sweep the flash limiter. Fired slots remain until the rolling window
    /// expires; a future slot remains only while its owner is resident and
    /// points at that start instant. Stale future work is cancelled before
    /// every grant, so the queue is bounded by live episode count.
    fn prune_ignitions(&mut self, now: Instant) {
        let persist = &self.persist;
        // The scope whose map `persist` currently is (the bound pane, else the
        // declared session), compared as an `Option` so "no scope" matches no
        // session.
        let scope = self.scan_scope();
        self.ignitions.retain(|reservation| {
            if reservation.start + IGNITION_WINDOW <= now {
                return false;
            }
            if reservation.start <= now {
                return true;
            }
            // Another scope's pending slot: its owner is parked out of reach.
            // Retaining it is correct (the nova is still queued) and safe (it
            // expires with the rolling window).
            if reservation.pane != scope {
                return true;
            }
            persist.get(&reservation.owner).is_some_and(|episode| {
                !episode.nova_done && episode.nova_start == Some(reservation.start)
            })
        });
        debug_assert!(
            self.ignitions.iter().filter(|r| r.pane == scope).count() <= MAX_IGNITION_RESERVATIONS
        );
    }

    /// §3.2 supernova prepass, before the classic one. For each rolled rainbow
    /// occurrence: grant ignition through the flash limiter, one supernova at a
    /// time (earlier wins), deferred while selected or while any classic window
    /// is live; expire finished windows; fill `supers`; and publish
    /// `super_until` from the persist-wide scan.
    fn super_prepass(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        sel: Option<SelView<'_>>,
    ) -> Option<Instant> {
        self.supers.clear();
        self.super_until = None;
        if cfg.reduced_motion {
            // reduced_motion suppresses the supernova outright (the word keeps
            // its static rainbow); perf_reduced degrades via freeze/thaw.
            return None;
        }
        self.prune_ignitions(now);
        let mut until: Option<Instant> = None;
        // Any granted, unfinished window holds the mutex, even before its
        // delayed start and while its word is off screen. Two-way: a live
        // classic nova blocks a supernova ignition, as a supernova blocks
        // classic grants, or the `nova_add` share could exceed MAX_NOVA_QUADS.
        // Parked panes' shards count too (the mutex is window-wide; the scope
        // census's supernova-burst-mutex aggregator checks this), and every
        // live end folds into the returned wake.
        let mut busy = false;
        // O(1) skip ([`Self::burst_hint`]): every end in this map is `<= hint`,
        // so once `now` reaches it the walk would change nothing. Debug builds
        // re-derive the answer and assert agreement; a low hint would let a
        // classic nova and a supernova overlap.
        if self.burst_hint.is_some_and(|h| now < h) {
            for ep in self.persist.values() {
                let Some(end) = burst_mutex_end(ep) else {
                    continue;
                };
                if now >= end {
                    continue;
                }
                busy = true;
                until = Some(until.map_or(end, |u: Instant| u.max(end)));
                if matches!(ep.burst_kind, Some(BurstKind::SuperNova)) {
                    self.super_until = Some(self.super_until.map_or(end, |u: Instant| u.max(end)));
                }
            }
        } else {
            debug_assert!(
                !self
                    .persist
                    .values()
                    .any(|ep| burst_mutex_end(ep).is_some_and(|end| now < end)),
                "a live burst window outlived the burst-mutex hint"
            );
        }
        // Parked shards contribute through their cached summary instead of a
        // walk of every pane's map per frame. Exact: `busy == (now < max(end))`
        // and `until`/`super_until` are that max; folding after the live walk
        // is fine since `busy` is an or and the others are max folds.
        for p in self.parked.values() {
            // The summaries are maintained only at [`ParkedPane::swap`]; this
            // debug cross-check re-derives the shard so a path bypassing `swap`
            // fails loudly. It stays inside this loop because the census check
            // is lexical: a second `self.parked` read would mask deletion of
            // the one below.
            debug_assert!(
                p.burst_summary() == (p.burst_end_max, p.super_end_max),
                "a parked shard's burst-mutex summary drifted from its episode map"
            );
            if let Some(e) = p.burst_end_max.filter(|e| now < *e) {
                busy = true;
                until = Some(until.map_or(e, |u: Instant| u.max(e)));
            }
            if let Some(e) = p.super_end_max.filter(|e| now < *e) {
                self.super_until = Some(self.super_until.map_or(e, |u: Instant| u.max(e)));
            }
        }
        // Hoisted: the reservation label depends on the binding, and
        // `scan_scope` borrows all of `self` while the loop holds `persist`.
        let scope = self.scan_scope();
        for (i, occ) in self.occ.iter().enumerate() {
            if occ.spec.burst.map(|b| b.kind) != Some(BurstKind::SuperNova) {
                continue;
            }
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                continue;
            };
            if !ep.burst_roll || ep.nova_done || ep.inert() {
                continue;
            }
            let ch = i32::from(geom.cell_h);
            let (cx, cy) = burst_center(occ, geom);
            // §3.2 reach: min(6 rows, grid extent).
            let grid_h = i32::from(geom.rows) * ch;
            let r_max = supernova::r_max_for(ch, grid_h);
            if ep.nova_start.is_none() {
                // Deferred while selected (§3.2 safety) or while another
                // supernova holds the mutex (MAX_ACTIVE_SUPERNOVAE = 1).
                if busy
                    || sel.is_some_and(|sv| sv.span_selected(occ.row, occ.start_col, occ.end_col))
                {
                    continue;
                }
                // The flash limiter charges a supernova as a full ignition.
                if !grant_burst(
                    &mut self.ignitions,
                    scope,
                    self.pane_px,
                    occ,
                    ep,
                    now,
                    (cx, cy),
                    r_max,
                    &mut self.burst_hint,
                    &mut self.curse_cues,
                ) {
                    continue;
                }
            }
            let Some(start) = ep.nova_start else { continue };
            let win = super_window_of(ep.burst_tier);
            if now >= start + win {
                ep.nova_done = true;
                continue;
            }
            busy = true;
            let end = start + win;
            until = Some(until.map_or(end, |u: Instant| u.max(end)));
            self.super_until = Some(self.super_until.map_or(end, |u: Instant| u.max(end)));
            if now < start {
                continue; // queued: the limiter delayed this ignition
            }
            if self.supers.len() < supernova::MAX_ACTIVE_SUPERNOVAE {
                self.supers.push(SuperLive {
                    idx: i as u16,
                    start,
                    center_px: (cx, cy),
                    r_max,
                });
            }
        }
        until
    }

    /// §6.4 nova prepass: sweep the limiter record, grant (possibly delayed)
    /// ignition slots in row-major order, expire finished windows into
    /// `nova_done` (one nova per episode), enforce the
    /// ≤ [`nova::MAX_ACTIVE_NOVAS`] cap (excess skips to Ember), fill this
    /// frame's `novas` scratch, and pick each nova's
    /// ≤ [`nova::MAX_COUPLING_WORDS`] nearest ink-bearing neighbors (§6.5).
    /// Returns the latest instant any granted window is still running (the
    /// scheduler keep-alive, covering delayed ignitions).
    fn nova_prepass(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        sel: Option<SelView<'_>>,
    ) -> Option<Instant> {
        self.novas.clear();
        self.coupling.clear();
        // Spent slots constrain for one rolling window; scheduled (future)
        // slots always survive. The supernova prepass already swept this
        // shared record, so this is a no-op here.
        self.prune_ignitions(now);
        if cfg.reduced_motion {
            return None;
        }
        // §3.2 global burst mutex: while a supernova is granted or live, all
        // classic ignitions queue behind its window end.
        let super_busy = self.super_until.is_some_and(|t| now < t);
        let mut until: Option<Instant> = None;
        // Hoisted for the same reason as in `super_prepass`: one binding, one
        // label, and `scan_scope` cannot be called while `persist` is borrowed.
        let scope = self.scan_scope();
        for (i, occ) in self.occ.iter().enumerate() {
            // v3 §6 dispatch: the classic nova is the `BurstKind::Nova` arm
            // (profanity `style = "nova"` and custom specs alike).
            if occ.spec.burst.map(|b| b.kind) != Some(BurstKind::Nova) {
                continue;
            }
            // Direct-built test occurrences without an episode take the ember
            // path (deterministic; the rescan always creates episodes).
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                continue;
            };
            if ep.nova_done {
                continue;
            }
            // Radius-only decode: not-yet-granted episodes take this arm every
            // frame while the limiter feeds out two grants per second; the full
            // genome decode waits for the `NovaLive` push below. Novas have no
            // size floor (they scale, §6.3).
            let ch = i32::from(geom.cell_h);
            let (cx, cy) = burst_center(occ, geom);
            let r_max = nova_radius(occ.genome.gkey) * ch.max(1) as f32;
            if ep.nova_start.is_none() {
                // `chance_pct`: a rolled-off burst never ignites (episodes with
                // a pre-granted `nova_start` are past the roll).
                if !ep.burst_roll {
                    continue;
                }
                // §6.4 item 6: no ignition while the word is selected —
                // deferred until deselection, then queued through the limiter.
                if sel.is_some_and(|sv| sv.span_selected(occ.row, occ.start_col, occ.end_col)) {
                    continue;
                }
                // Burst mutex: defer behind a live supernova, keeping the
                // scheduler armed one beat past its window so the grant happens.
                if super_busy {
                    if let Some(su) = self.super_until {
                        let wake = su + Duration::from_millis(1);
                        until = Some(until.map_or(wake, |u: Instant| u.max(wake)));
                    }
                    continue;
                }
                if !grant_burst(
                    &mut self.ignitions,
                    scope,
                    self.pane_px,
                    occ,
                    ep,
                    now,
                    (cx, cy),
                    r_max,
                    &mut self.burst_hint,
                    &mut self.curse_cues,
                ) {
                    continue;
                }
            }
            let Some(start) = ep.nova_start else { continue };
            let d = Duration::from_millis(u64::from(nova_duration_ms(occ.genome.gkey)));
            if now >= start + d {
                // The window elapsed: the episode's one nova is spent (§3.6).
                ep.nova_done = true;
                continue;
            }
            until = Some(until.map_or(start + d, |u: Instant| u.max(start + d)));
            if now < start {
                continue; // Armed: the limiter delayed this Dip start
            }
            if self.novas.len() >= nova::MAX_ACTIVE_NOVAS {
                // §6.3: excess concurrent novas skip straight to Ember
                // (row-major order — earlier occurrences win the slots).
                ep.nova_done = true;
                continue;
            }
            self.novas.push(NovaLive {
                idx: i as u16,
                start,
                center_px: (cx, cy),
                r_max,
                // The one full decode on this path, per live nova.
                feats: nova_features(occ.genome.gkey),
                magic: if cfg.profanity_magic {
                    nova_magic(occ.genome.magic)
                } else {
                    None
                },
            });
        }
        // §6.5 coupling: per live nova, the MAX_COUPLING_WORDS nearest
        // ink-bearing occurrences, sorted by `(d2, index)` (row-major ties).
        for (ni, nv) in self.novas.iter().enumerate() {
            self.dist_scratch.clear();
            for (i, occ) in self.occ.iter().enumerate() {
                if i == usize::from(nv.idx) || occ.ink_cells == 0 {
                    continue;
                }
                self.dist_scratch
                    .push((span_dist2(occ, nv.center_px, geom), i as u16));
            }
            self.dist_scratch.sort_unstable();
            for &(_, i) in self.dist_scratch.iter().take(nova::MAX_COUPLING_WORDS) {
                self.coupling.push((i, ni as u8));
            }
        }
        until
    }

    /// Whether any occurrence is still inside its animation window (profanity
    /// sparkle, a live ink sweep, or a granted, possibly queued, nova window).
    /// The scheduler keeps presenting frames while true. Cheap: reads the
    /// deadline `tick` last computed. Every arm is an episode, and episodes are
    /// pruned against the visible layout by `retain_panes`, so no per-session
    /// filter is needed.
    pub fn is_active(&self, now: Instant) -> bool {
        self.active_until.is_some_and(|d| now < d)
            || self
                .parked
                .values()
                .any(|p| p.active_until.is_some_and(|d| now < d))
    }
}

/// Pixel center of a burst word's span. DEC rows anchor via the row's real
/// advance (2× on DECDWL). Shared by both burst prepasses.
fn burst_center(occ: &Occurrence, geom: EffectGeom) -> (i32, i32) {
    let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
    let ch = i32::from(geom.cell_h);
    let cx = (i32::from(occ.start_col) + i32::from(occ.end_col) + 1) * advance / 2;
    let cy = i32::from(occ.row) * ch + ch / 2;
    (cx, cy)
}

/// The ignition grant both burst prepasses share, the point of no return.
/// Requests the §6.4 flash slot (overlap distance `2·r_max`); when granted it
/// latches `nova_start`, folds the window into the burst-mutex hint, sets
/// `burst_done` (a burst scrolled off mid-blast never replays) and, for
/// profanity, queues the `Detonated` bonk cue. `false` when refused (nothing
/// written). A free function so the caller keeps its `&mut persist` borrow.
#[allow(
    clippy::too_many_arguments,
    reason = "the prepass's disjoint field borrows, split around its live `&mut persist` entry"
)]
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
        *burst_hint = Some(burst_hint.map_or(end, |h: Instant| h.max(end)));
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

/// Request a §6.4 flash slot for `pane`, whose grid origin sits at `pane_px`
/// in window pixels. The limiter is window-wide, so every pane's centers are
/// shifted into one coordinate space for the overlap test. A free function so
/// the caller keeps its disjoint `&mut persist` borrow.
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

/// §6.4 window-wide ignition limiter: the earliest slot ≥ `now` with fewer than
/// 2 ignitions in the trailing rolling second, or fewer than 1 when a counted
/// region overlaps the candidate's (center distance < 2·R_max). Row-major
/// callers make the delays a deterministic queue; each episode requests at
/// most one slot, after stale future owners are pruned
/// ([`WordDecorations::prune_ignitions`]). The hard cap fails closed (`None`)
/// rather than drop safety history. Modeled as `FlashLimiter` and
/// `IgnitionReservationLifecycle`. `center` is in window pixels.
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
    if igns.iter().filter(|r| r.pane == pane).count() >= MAX_IGNITION_RESERVATIONS {
        return None;
    }
    let mut slot = now;
    // Each pass either admits the slot or advances it to the earliest expiry
    // of a blocking record — monotone, so it terminates in ≤ len passes.
    loop {
        let mut count = 0usize;
        let mut overlap = false;
        let mut next_free: Option<Instant> = None;
        for reservation in igns.iter() {
            let (t, c) = (reservation.start, reservation.center);
            if t > slot || t + IGNITION_WINDOW <= slot {
                continue; // outside the rolling second ending at `slot`
            }
            count += 1;
            let (dx, dy) = ((c.0 - center.0) as f32, (c.1 - center.1) as f32);
            if (dx * dx + dy * dy).sqrt() < overlap_dist {
                overlap = true;
            }
            let free = t + IGNITION_WINDOW;
            next_free = Some(next_free.map_or(free, |f: Instant| f.min(free)));
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

/// §6 classic nova (`style = "nova"` and custom `kind = "nova"`), the
/// `BurstKind::Nova` arm of tick's burst dispatch. Emission is a pure function
/// of (now − nova_start, genome, geometry); stored state (grant, nova_done)
/// lives in the episode and limiter record, mutated only by the prepass.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
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
    if let Some(nv) = novas.iter().find(|n| usize::from(n.idx) == oi).copied() {
        let t = now.saturating_duration_since(nv.start).as_millis() as u64;
        // The live window keeps the scheduler armed and the fp changing every
        // frame, so the present early-out never skips a live frame.
        let until = nv.start + Duration::from_millis(u64::from(nv.feats.duration_ms));
        arm_until(active_until, until);
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        let env = nova_env(occ, &nv, geom, cfg);
        // Per-nova budget under the decoration backstop (which
        // a genome-reachable frame never binds, §6.3).
        let budget = nova::MAX_NOVA_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        nova::emit_nova(t, &env, budget, nova);
        // §6.4: an active nova over a selection is attenuated with the same
        // per-cell predicate as Add decos, applied per quad (center cell)
        // host-side so both backends stay byte-identical.
        if let Some(sv) = sel
            && sv.sel.has_selection()
        {
            let cw = i32::from(geom.cell_w).max(1);
            let mut w = n0;
            for r in n0..nova.len() {
                let q = nova[r];
                let ccol = ((i32::from(q.x) + i32::from(q.w) / 2) / cw).max(0) as u16;
                if !sv.cell_selected(q.row, ccol) {
                    nova[w] = q;
                    w += 1;
                }
            }
            nova.truncate(w);
        }
        for q in &nova[n0..] {
            *fp = fold_glow(*fp, q);
        }
        // Debris rides the Add deco stream (§6.1), inheriting the selection
        // freeze and additive parity; the Singularity's darkening ring rides
        // the Over stream as per-cell RingArc masks (additive light can only
        // brighten).
        let d0 = out.len();
        nova::emit_debris(t, &env, out, MAX_DECORATIONS);
        if nv.magic == Some(NovaMagic::Singularity) {
            let cap = nova::MAX_RING_ARC_CELLS.min(MAX_DECORATIONS.saturating_sub(out.len()));
            nova::emit_ring_arc(t, &env, out, cap);
        }
        for d in &out[d0..] {
            *fp = fold_deco(*fp, d);
        }
    } else {
        // Ember / Settled, and reduced_motion's static glint from frame 0
        // (§6.4): one dim residual spark in the palette's ember tone (dim violet
        // for the Singularity). A deferred ignition emits nothing yet. The
        // ember fades out within 2 s of the window's end, then nothing; the
        // reduced-motion static glint keeps fixed bytes.
        let done = ep.is_none_or(|e| e.nova_done);
        let fade = if cfg.reduced_motion && !occ.inert {
            // Under reduced motion the static glint must not resurrect a
            // finished one-shot: once the non-reduced path emits zero (nova
            // played + ember fade elapsed), so does reduced. A never-ignited
            // episode keeps the static glint; a rolled-off one shows nothing.
            let rolled_off = ep.is_some_and(|e| !e.burst_roll);
            let spent = ep.is_some_and(|e| {
                e.nova_done
                    && e.nova_start.is_some_and(|s| {
                        now >= s + Duration::from_millis(
                            u64::from(nova_duration_ms(occ.genome.gkey)) + RESIDUAL_FADE_MS,
                        )
                    })
            });
            (!rolled_off && !spent).then_some(1.0f32)
        } else if done && !occ.inert {
            ep.and_then(|e| e.nova_start).and_then(|s| {
                // Duration-only decode: every finished nova on screen asks
                // whether its ember fade elapsed, every frame.
                let end = s + Duration::from_millis(u64::from(nova_duration_ms(occ.genome.gkey)));
                let t = now.saturating_duration_since(end).as_millis() as u64;
                (t < RESIDUAL_FADE_MS).then(|| {
                    let f = 1.0 - t as f32 / RESIDUAL_FADE_MS as f32;
                    let until = end + Duration::from_millis(RESIDUAL_FADE_MS);
                    arm_until(active_until, until);
                    f
                })
            })
        } else {
            None
        };
        if let Some(f) = fade {
            let magic = if cfg.profanity_magic {
                nova_magic(occ.genome.magic)
            } else {
                None
            };
            // Palette-only decode: the residual star reads one field, not nine.
            let (_, ember) = nova::ember_pair(nova::palette(nova_palette(occ.genome.gkey)), magic);
            let d = WordDecoration {
                row: occ.row,
                col: occ.start_col,
                dx: 0,
                // Lifted off the glyph body so the star does not overlap the
                // word's strokes.
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

/// §3.2 supernova (rolled rainbow episodes), the `BurstKind::SuperNova` arm of
/// tick's burst dispatch. Emission is a pure function of (now − nova_start,
/// env); the roll, grant and mutex live in the prepass.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
fn emit_super_axis(
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
    if let Some(sv) = supers.iter().find(|s| usize::from(s.idx) == oi).copied() {
        let t = now.saturating_duration_since(sv.start).as_millis() as u64;
        // Per-tier window: Flash ~1.1 s, Nova 2.4 s, Nuke 3.6 s.
        let tier = ep.map_or(supernova::SuperTier::Nova, |e| e.burst_tier);
        let until = sv.start + Duration::from_millis(supernova::total_ms(tier));
        arm_until(active_until, until);
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        // §3.2 theme branch: additive white is invisible on light backgrounds,
        // so the eclipse rides the Over deco stream there.
        let light = relative_luminance(rgb3_to_u32(occ.ink_bg)) > 0.5;
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
        // Own budget under the decoration backstop; the burst mutex (held for
        // the full window, on-screen or not) keeps this pass's share ≤ 1536, so
        // the clamp never binds. Prism wake appends after this pass
        // (`nova::MAX_NOVA_QUADS`).
        let budget =
            supernova::MAX_SUPER_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        supernova::emit_super(t, &env, budget, nova);
        // The nuke cloud: recorded here, emitted after the occurrence loop
        // (baking needs `&mut self.cat_baker`). At most one cloud exists.
        if tier == supernova::SuperTier::Nuke && !cfg.reduced_motion {
            *nuke_pending = Some((t, env.cx, env.cy));
        }
        // §3.2 selection × wash: split row quads around the selected span (the
        // center-cell predicate is degenerate for full-width wash quads).
        // Host-side, so CPU and GPU stay byte-identical.
        if let Some(sv_sel) = sel
            && sv_sel.sel.has_selection()
        {
            split_super_selection(nova, n0, geom, &sv_sel);
        }
        for q in &nova[n0..] {
            *fp = fold_glow(*fp, q);
        }
        let d0 = out.len();
        supernova::emit_super_decos(t, &env, out, MAX_DECORATIONS);
        // §3.2 selection × Over stream: every supernova Over deco (the eclipse
        // veil, light-theme charge motes and debris) is dropped over selected
        // cells, since the renderers' selection freeze covers only the Add
        // stream. Same per-cell predicate, applied host-side.
        if let Some(sv_sel) = sel
            && sv_sel.sel.has_selection()
        {
            let mut w = d0;
            for r in d0..out.len() {
                let dq = out[r];
                if !(matches!(dq.blend, DecoBlend::Over) && sv_sel.cell_selected(dq.row, dq.col)) {
                    out[w] = dq;
                    w += 1;
                }
            }
            out.truncate(w);
        }
        for d in &out[d0..] {
            *fp = fold_deco(*fp, d);
        }
    } else if !cfg.reduced_motion
        && let Some(ep) = ep
        && ep.burst_roll
        && let Some(s) = ep.nova_start
    {
        // Afterglow: the ink settles to the static rainbow; the ember star fades
        // within 2 s after this tier's window (§3.2), then nothing.
        let end = s + Duration::from_millis(supernova::total_ms(ep.burst_tier));
        if now >= end {
            let t = now.saturating_duration_since(end).as_millis() as u64;
            if t < RESIDUAL_FADE_MS {
                let f = 1.0 - t as f32 / RESIDUAL_FADE_MS as f32;
                let until = end + Duration::from_millis(RESIDUAL_FADE_MS);
                arm_until(active_until, until);
                let d = WordDecoration {
                    row: occ.row,
                    col: occ.start_col,
                    dx: 0,
                    dy: -((i32::from(geom.cell_h) / 3).min(127) as i8),
                    glyph: DecoGlyph::Star4,
                    blend: DecoBlend::Add,
                    // The one gold: the warm ember over the rainbow.
                    color: crate::effect_util::twinkle_rgb(true),
                    alpha: scale_u8(cfg.intensity * 0.30 * f),
                };
                *fp = fold_deco(*fp, &d);
                out.push(d);
            }
        }
    }
}

/// §3.2 selection × wash: split freshly-emitted supernova quads
/// (`quads[n0..]`) around each quad's selected cell span. A full-width wash
/// quad over a mid-screen selection becomes its left and right remainders
/// (≤ 3 quads/row, charged in the S_max closed form); a fully selected quad
/// drops. Host-side, so CPU and GPU stay byte-identical.
///
/// Known limitation: columns use the base cell width, so on a DECDWL row the
/// hole is cut at half scale and a selection there may stay washed during the
/// ≤ 300 ms wash. Accepted to keep the split and its accounting simple.
fn split_super_selection(quads: &mut Vec<GlowQuad>, n0: usize, geom: EffectGeom, sv: &SelView<'_>) {
    let cw = i32::from(geom.cell_w).max(1);
    let cols = i32::from(geom.cols).max(1);
    let mut i = n0;
    while i < quads.len() {
        let q = quads[i];
        let c0 = (i32::from(q.x) / cw).clamp(0, cols - 1);
        let c1 = ((i32::from(q.x) + i32::from(q.w) - 1) / cw).clamp(0, cols - 1);
        let (mut s0, mut s1) = (None, None);
        for c in c0..=c1 {
            if sv.cell_selected(q.row, c as u16) {
                if s0.is_none() {
                    s0 = Some(c);
                }
                s1 = Some(c);
            }
        }
        let (Some(s0), Some(s1)) = (s0, s1) else {
            i += 1;
            continue;
        };
        // Cut the [s0, s1] cell hole out of the quad: one contiguous hole (≤ 2
        // remainders). Disjoint selected runs collapse into their bounding
        // hole, which only clears more.
        let (hx0, hx1) = (s0 * cw, (s1 + 1) * cw);
        let (qx0, qx1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        // The `..q` spreads carry the quad's colour pair across the split,
        // correct only for flat quads; every nova quad is flat.
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
            quads.remove(i); // fully selected: the quad drops
        }
    }
}

/// Squared px distance from `center` to the nearest point of the occurrence's
/// visual span (along its row-band center line) — the §6.5 coupling metric
/// and the ring-crossing distance.
fn span_dist2(occ: &Occurrence, center: (i32, i32), geom: EffectGeom) -> u64 {
    let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
    let ch = i32::from(geom.cell_h);
    let x0 = i32::from(occ.start_col) * advance;
    let x1 = (i32::from(occ.end_col) + 1) * advance;
    let y = i32::from(occ.row) * ch + ch / 2;
    let dx = (x0 - center.0).max(center.0 - x1).max(0);
    let dy = (center.1 - y).abs();
    (i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)) as u64
}

/// Resolve one live nova's pure-emitter environment (§6.3 geometry + §6.2
/// hue-nudged palette endpoints) from the occurrence + frame geometry.
fn nova_env(occ: &Occurrence, nv: &NovaLive, geom: EffectGeom, cfg: &DecoConfig) -> NovaEnv {
    let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
    let (core, fringe) = nova::palette(nv.feats.palette);
    let (n0, n1) = genome::ink_pair_nudges(Class::Profanity, occ.genome.gkey);
    NovaEnv {
        grid_w: i32::from(geom.cols) * i32::from(geom.cell_w),
        grid_h: i32::from(geom.rows) * i32::from(geom.cell_h),
        cell_w: advance.max(1),
        cell_h: i32::from(geom.cell_h).max(1),
        cx: nv.center_px.0,
        cy: nv.center_px.1,
        r_max: nv.r_max,
        feats: nv.feats,
        magic: nv.magic,
        core: hue_nudge(core, n0),
        fringe: hue_nudge(fringe, n1),
        intensity: cfg.intensity,
        seed: occ.seed,
    }
}

/// The §6 per-frame ink modifiers for one occurrence, everything the nova
/// machinery folds into the ink channel. Pure function of (occurrence, live
/// novas, episode, now); [`INK_FX_NONE`] means no modification.
struct InkFx {
    /// Anchor-pair override: the nova palette endpoints for a nova-style
    /// profanity word (ember-shifted once spent, §6.1), chroma-pulsed by §6.5
    /// coupling. `None` means the class default pair.
    pair: Option<(u32, u32)>,
    /// §6.1 Dip: the ink envelope multiplier (`1.0` outside the dip).
    dim: f32,
    /// §6.4: the word's own sweep is held at its settled gradient during its
    /// nova window, so the dip/flash pair is the only ink transition then.
    freeze: bool,
    /// Ink bytes are changing per frame (dip ramp / coupling pulse): fold the
    /// frame term so the present early-out never skips a live frame.
    frame_live: bool,
    /// §3.2 supernova charge: `0..=1` drive of the ink toward white-hot (dark
    /// bg) or near-black (light bg), rising through the charge, holding through
    /// the detonation, decaying by debris start. `0.0` outside a supernova.
    lift: f32,
}

const INK_FX_NONE: InkFx = InkFx {
    pair: None,
    dim: 1.0,
    freeze: false,
    frame_live: false,
    lift: 0.0,
};

/// Build the [`InkFx`] for occurrence `oi` this frame: the own-nova arm
/// (§6.1 palette anchors / dip / freeze / ember) plus the §6.5 blast-coupling
/// chroma pulse when a live nova's ring is crossing this word's span.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence resolution over tick-local nova scratch, the emit_ink idiom"
)]
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
) -> InkFx {
    let mut fx = INK_FX_NONE;
    // v3 §3.2: a live supernova drives its own word's ink (charge → hold →
    // decay); the rainbow branch applies the theme-correct direction.
    for s in supers {
        if usize::from(s.idx) != oi {
            continue;
        }
        let t = now.saturating_duration_since(s.start).as_millis() as u64;
        let lift = if t < supernova::CHARGE_END_MS {
            t as f32 / supernova::CHARGE_END_MS as f32
        } else if t < supernova::DETONATION_END_MS {
            1.0
        } else if t < supernova::DEBRIS_START_MS {
            1.0 - (t - supernova::DETONATION_END_MS) as f32
                / (supernova::DEBRIS_START_MS - supernova::DETONATION_END_MS) as f32
        } else {
            0.0
        };
        if lift > 0.0 {
            fx.lift = lift;
            fx.frame_live = true;
        }
        break;
    }
    if occ.spec.burst.map(|b| b.kind) == Some(BurstKind::Nova) {
        // Palette/duration-only decodes: this runs per ink-bearing nova word
        // per frame and reads two fields of a fixed genome.
        let magic = if cfg.profanity_magic {
            nova_magic(occ.genome.magic)
        } else {
            None
        };
        let (core, fringe) = nova::palette(nova_palette(occ.genome.gkey));
        let (n0, n1) = genome::ink_pair_nudges(Class::Profanity, occ.genome.gkey);
        let pair = (hue_nudge(core, n0), hue_nudge(fringe, n1));
        let done = ep.is_none_or(|e| e.nova_done);
        if cfg.reduced_motion || done {
            // §6.1 Ember: the settled gradient shifted to the palette's ember
            // tone (reduced_motion takes it from frame 0 — the static glint).
            fx.pair = Some(nova::ember_pair(pair, magic));
        } else {
            // §4.2: profanity ink = the nova palette endpoints.
            fx.pair = Some(pair);
            if let Some(start) = ep.and_then(|e| e.nova_start)
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
    // §6.5 blast coupling: a one-shot ~150 ms constant-luminance chroma pulse
    // toward the nova palette as the ring crosses this word's span; a pure
    // function of (nova center, t, span). The first coupled nova in prepass
    // order wins.
    for &(ci, ni) in coupling {
        if usize::from(ci) != oi {
            continue;
        }
        let nv = &novas[usize::from(ni)];
        let t = now.saturating_duration_since(nv.start).as_millis() as u64;
        let dist = (span_dist2(occ, nv.center_px, geom) as f32).sqrt();
        let sing = nv.magic == Some(NovaMagic::Singularity);
        if let Some(cross) = nova::crossing_ms(dist, nv.r_max, sing)
            && let Some(env) = nova::pulse_env(t, cross)
        {
            let base = fx
                .pair
                .or_else(|| nudged_ink_pair(occ.class, occ.genome.gkey));
            if let Some((c0, c1)) = base {
                let (_, fringe) = nova::palette(nv.feats.palette);
                let amp = nova::PULSE_AMP * env;
                // Singularity inverts (§6.5): the gradient leans into the
                // collapse; the anchor facing the nova pulses at full amplitude.
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
                // Constant luminance (§6.4): the applied color is
                // luminance-matched back to its anchor, so |ΔL| ≤ 5 % holds.
                fx.pair = Some((
                    nova::pulse_color(c0, fringe, a0),
                    nova::pulse_color(c1, fringe, a1),
                ));
                fx.frame_live = true;
            }
            break;
        }
    }
    fx
}

/// Resolved §5.2 cat geometry for one occurrence (pure; unit-tested).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CatGeom {
    /// Authored-aspect design-box width in px, capped by the atlas slot.
    w: u16,
    /// Uniformly age-scaled art height, capped at the two-row atlas band.
    hart: u16,
    /// Chin slice `floor(0.15·Hart)` — the part spilling into the word's row.
    chin: u16,
    /// Viewport-clamped left dest edge (px-midpoint anchor, §5.2).
    x: u16,
}

// ───────── §5.2 pub geometry helpers (shared with the demo and gallery) ─────────

/// §5.2 base art height before age scaling: `Hart = round(1.7·ch)`. The fitted
/// result stays inside the two-row band above the word plus its chin slice.
pub fn cat_hart(cell_h: u16) -> u16 {
    ((1.7 * f32::from(cell_h)).round() as u16).max(1)
}

/// §5.2 chin slice: `floor(0.15·Hart)` — the part spilling into the word row
/// (occluded by the word's ascenders: the cat is visibly behind the word).
pub fn cat_chin(hart: u16) -> u16 {
    (0.15 * f32::from(hart)).floor() as u16
}

/// Fit one authored cat into the atlas slot at a requested height, preserving
/// its viewbox aspect; when the slot binds, both dimensions shrink together.
fn authored_cat_size(variant: CatGlyphId, desired_h: f32, cell_h: u16) -> (u16, u16) {
    let max_w = cell_h.saturating_mul(4).saturating_sub(PATCH_STRIP);
    fit_glyph(&GLYPHS[variant as usize], desired_h, max_w, cell_h)
}

/// Fit an authored glyph (cat or species head) at a requested height into a
/// `max_w × 2·cell_h` box, preserving its viewbox aspect: when the width cap
/// binds, both dimensions shrink together.
fn fit_glyph(glyph: &GlyphDef, desired_h: f32, max_w: u16, cell_h: u16) -> (u16, u16) {
    let aspect = (f32::from(glyph.aspect_x1000) / 1000.0).max(0.001);
    let max_w = f32::from(max_w.max(1));
    let max_h = f32::from(cell_h.saturating_mul(2).max(1));
    let fitted_h = desired_h.max(1.0).min(max_h).min(max_w / aspect);
    let h = fitted_h.round().clamp(1.0, max_h) as u16;
    let w = (f32::from(h) * aspect).round().clamp(1.0, max_w) as u16;
    (w, h)
}

/// §5.6 rest reveal: full Hart for every identity — bobs and entrances
/// terminate at the same fixed point for everyone.
pub fn cat_rest_reveal(hart: u16) -> u16 {
    hart
}

/// cat-art v4 eligibility gate: the cat `feline_style` plus [`peek_eligible`].
fn cat_eligible(occ: &Occurrence, cfg: &DecoConfig, geom: EffectGeom) -> bool {
    cfg.feline_style == FelineStyle::Cat && peek_eligible(occ, geom)
}

/// The peek eligibility both rosters share: the word is wide enough
/// (`word_px ≥ 0.8·ch`, so a 1-cell word gets none), [`cat_peek_plan`] found a
/// habitable side, and the cell metrics clear the §5.7 floors. Top rows are
/// eligible (the plan picks [`PeekDir::Down`]); busy surfaces too, since heads
/// draw [`FreeZ::UnderText`] and only a text wall on both sides is rejected.
/// Ineligible words draw no graphic, just ink.
fn peek_eligible(occ: &Occurrence, geom: EffectGeom) -> bool {
    if geom.cell_h < CAT_MIN_CELL_H
        || geom.cell_w < CAT_MIN_CELL_W
        || occ.dec_line
        || !occ.cat_text_clear
    {
        return false;
    }
    let word_px = (i32::from(occ.end_col) - i32::from(occ.start_col) + 1) * i32::from(geom.cell_w);
    (word_px as f32) >= 0.8 * f32::from(geom.cell_h)
}

/// §5.2 geometry: uniformly age-scaled authored aspect, word-midpoint anchor,
/// and viewport clamp. v4 cats occupy at most the existing two-row atlas band.
fn cat_geometry(occ: &Occurrence, geom: EffectGeom, variant: CatGlyphId) -> CatGeom {
    cat_geometry_for(occ.start_col, occ.end_col, occ.genome.gkey, geom, variant)
}

fn cat_geometry_for(
    start_col: u16,
    end_col: u16,
    gkey: u64,
    geom: EffectGeom,
    variant: CatGlyphId,
) -> CatGeom {
    let desired_h = f32::from(cat_hart(geom.cell_h)) * cat_age_v4(gkey).scale();
    let size = authored_cat_size(variant, desired_h, geom.cell_h);
    anchored_geom(start_col, end_col, geom, size, &GLYPHS[variant as usize])
}

/// Land a fitted `(w, hart)` glyph's authored visual center on the word
/// midpoint, then clamp inward at viewport edges.
fn anchored_geom(
    start_col: u16,
    end_col: u16,
    geom: EffectGeom,
    (w, hart): (u16, u16),
    glyph: &GlyphDef,
) -> CatGeom {
    let cw = i32::from(geom.cell_w);
    let w_i = i32::from(w);
    let grid_w = i32::from(geom.cols) * cw;
    let mid = (i32::from(start_col) * cw + (i32::from(end_col) + 1) * cw) / 2;
    let center = f32::from(glyph.center_x) / f32::from(FIXED_ONE);
    let x = (mid - (center * f32::from(w)).round() as i32).clamp(0, (grid_w - w_i).max(0));
    CatGeom {
        w,
        hart,
        chin: cat_chin(hart),
        x: x as u16,
    }
}

/// §5.2 geometry for a species head: the cat law without the age scale (every
/// animal is an adult) or the patch strip in the width cap (animal tiles are
/// exact-size). Authored aspect, word-midpoint anchor, viewport clamp.
fn animal_geometry(occ: &Occurrence, geom: EffectGeom, species: AnimalGlyphId) -> CatGeom {
    let glyph = &ANIMAL_GLYPHS[species as usize];
    // `host_tile` admits `w ≤ slot_w (= 4·cell_h)` and `h ≤ 2·cell_h`.
    let max_w = geom.cell_h.saturating_mul(4);
    let size = fit_glyph(glyph, f32::from(cat_hart(geom.cell_h)), max_w, geom.cell_h);
    anchored_geom(occ.start_col, occ.end_col, geom, size, glyph)
}

/// §5.6 ease-out-back with an exact overshoot amplitude: `f(p) = 1 +
/// (c+1)(p−1)³ + c(p−1)²`, `c` solved from the closed form
/// `amp = 4c³ / (27(c+1)²)` by bisection so `max f = 1 + amp` (the genome
/// amplitude, kitten-scaled ×1.3). `f(0) = 0`, `f(1) = 1`.
fn ease_out_back(p: f32, amp: f32) -> f32 {
    let c = overshoot_c(amp);
    let u = p - 1.0;
    1.0 + (c + 1.0) * u * u * u + c * u * u
}

/// Solve `amp = 4c³ / (27(c+1)²)` for `c`, memoized on the exact `f32` bits of
/// `amp`.
///
/// The bisection (24 rounds on `[0, 6]`) must stay as it is: the closed-form
/// root differs by a few ULPs, which moves the entrance overshoot by a pixel
/// on some frames of some genomes. `amp` is frame-invariant and takes one of
/// eight values (`(0.06 + a 2-bit genome field × 0.04) × {1.0, 1.3}` in
/// [`emit_peek`]), so a memo avoids re-solving per frame.
fn overshoot_c(amp: f32) -> f32 {
    // Past the reachable set with headroom; a ninth amplitude (only a custom
    // spec could produce one) misses and pays the solve.
    const SLOTS: usize = 16;
    thread_local! {
        static MEMO: std::cell::RefCell<[(u32, f32); SLOTS]> =
            const { std::cell::RefCell::new([(0, 0.0); SLOTS]) };
    }
    // Key 0 marks an empty slot. `amp = +0.0` is not a reachable amplitude (the
    // smallest is 0.06), so nothing real is denied a slot by that choice; a
    // zero amplitude just never caches.
    let key = amp.to_bits();
    if key != 0
        && let Some(c) =
            MEMO.with_borrow(|memo| memo.iter().find_map(|&(k, c)| (k == key).then_some(c)))
    {
        return c;
    }
    let over = |c: f32| 4.0 * c * c * c / (27.0 * (c + 1.0) * (c + 1.0));
    let (mut lo, mut hi) = (0.0f32, 6.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if over(mid) < amp { lo = mid } else { hi = mid }
    }
    let c = 0.5 * (lo + hi);
    if key != 0 {
        MEMO.with_borrow_mut(|memo| {
            if let Some(slot) = memo.iter_mut().find(|(k, _)| *k == 0) {
                *slot = (key, c);
            }
        });
    }
    c
}

/// v3 §1.2 descend easing (easeInOutCubic), `f(0) = 0`, `f(1) = 1`.
fn ease_in_out_cubic(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p < 0.5 {
        4.0 * p * p * p
    } else {
        let u = -2.0 * p + 2.0;
        1.0 - u * u * u / 2.0
    }
}

// ───────── v3 §1.2 peek-cycle timing (pure fns of genome + ident) ─────────
//
// These decode as salted mixes of `gkey` rather than dedicated genome bits;
// see [`DWELL_SALT`].

/// Dwell base over the spec range (`2200..=3598 ms` default) in 4 genome
/// steps (step `(hi − lo)/3` — 466 ms at the default range).
fn dwell_base_ms(gkey: u64, range: (u32, u32)) -> u64 {
    let lo = u64::from(range.0.min(range.1));
    let hi = u64::from(range.0.max(range.1));
    lo + (mix(gkey ^ DWELL_SALT) % 4) * ((hi - lo) / 3)
}

/// The episode's dwell: base − `mix(ident) % 300` twin-desync jitter,
/// + 500 ms for magic/accessory episodes, capped at 3750 ms.
fn peek_dwell_ms(ident: u64, gkey: u64, magical: bool, range: (u32, u32)) -> u64 {
    let mut d = dwell_base_ms(gkey, range).saturating_sub(mix(ident) % CAT_DWELL_JITTER_MS);
    if magical {
        d += CAT_DWELL_MAGIC_BONUS_MS;
    }
    d.min(CAT_DWELL_CAP_MS)
}

/// The optional 60 ms pre-descend anticipation lift (~50% of genomes).
fn anticipation_ms(gkey: u64) -> u64 {
    if mix(gkey ^ ANTIC_SALT) & 1 == 0 {
        CAT_ANTICIPATION_MS
    } else {
        0
    }
}

/// v4 §3 accessory presence (bow/crown/bell): the dwell bonus honors it exactly
/// like magic. The decode is structurally `None` on special-build cats, so the
/// bonus never double-counts a special.
fn has_accessory(magic: u64) -> bool {
    accessory_variant_v4(magic).is_some()
}

/// Total peek length: Rise + Dwell + optional anticipation + Descend. Worst
/// case `450 + 3750 + 60 + 320 = 4580 ms` ([`CAT_DWELL_CAP_MS`] binds custom
/// spec ranges and rare cats too).
fn peek_total_ms(ident: u64, gkey: u64, magical: bool, range: (u32, u32)) -> u64 {
    CAT_RISE_MS
        + peek_dwell_ms(ident, gkey, magical, range)
        + anticipation_ms(gkey)
        + CAT_DESCEND_MS
}

/// The caret sits on the token span (or in the cell right after it). The one
/// span predicate shared by the ambiguous-prefix defer, the `at_live_cursor`
/// retype witness, and the one-cat-per-caret suppression — features that must
/// stay in agreement about what "at the caret" means.
fn caret_on_span(cell: (u16, u16), row: u16, start_col: u16, end_col: u16) -> bool {
    let (caret_row, caret_col) = cell;
    caret_row == row && caret_col >= start_col && caret_col <= end_col.saturating_add(1)
}

/// Whether the trick flash may paint under `cfg` — the SelfGlow law. Ink off
/// means zero [`InkCell`]s for everyone; and reduced motion never arms the
/// scheduler, so it could never take a transient tint off again: no flash.
fn trick_flash_paintable(cfg: &DecoConfig) -> bool {
    cfg.ink_enabled && !cfg.reduced_motion
}

/// Arm (or extend) the shared animation deadline to at least `until`, keeping
/// the repaint scheduler awake through the latest live window.
fn arm_until(active_until: &mut Option<Instant>, until: Instant) {
    *active_until = Some(active_until.map_or(until, |d| d.max(until)));
}

/// Done-mark write (v3 §1.1): insert (or refresh) the episode's mark under the
/// ident current at departure. Keyed `ident ^ ctx_fp`; `ctx_fp` is stored
/// because a horizontal redraw can rekey the position-bearing seed while the
/// frozen genome stays unchanged.
fn mark_done(marks: &mut DoneMarkLru, ident: u64, ep: &Episode) {
    let key = ident ^ ep.ctx_fp;
    marks.insert(key);
}

/// Done-mark lookup with touch-on-hit (LRU refresh).
fn touch_done(marks: &mut DoneMarkLru, key: u64) -> bool {
    marks.touch(key)
}

/// The ambient cat's idle life during the dwell: a slow genome-phased bob and
/// a slower lateral sway, small on purpose.
///
/// A pure function of the dwell-relative clock and the genome, not
/// [`crate::kitty_cursor::CatPose`] (stateful, driven by a typing metric a
/// scanned word lacks): no `Episode` field, no new bake key (the eye frame
/// stays `Open`), and `emit_peek` stays pure. Pure translation, so the dest
/// size pinned by `shipping_cat_sprites_preserve_authored_aspect_and_uniform_age_scale`
/// and `one_shot_peek_conformance_real_engine` is unchanged.
#[derive(Clone, Copy)]
struct CatIdlePose {
    bob_px: i32,
    lead_px: i32,
}

impl CatIdlePose {
    /// The neutral pose — the exact pre-animation sprite, byte for byte.
    const STILL: Self = Self {
        bob_px: 0,
        lead_px: 0,
    };

    /// `td_ms` is the dwell-relative clock (zero at the end of the rise), so the
    /// entrance and the exit keep their authored kinematics exactly.
    fn breathing(td_ms: u64, gkey: u64, cell_h: u16) -> Self {
        // A whole device pixel either way on a ~1.7-cell-tall head: enough to
        // read as breathing at a glance, small enough to sit under reading.
        let amp = (f32::from(cell_h) * 0.06).clamp(1.0, 2.0);
        // Genome-phased periods, bounded so every cat reads as the same species
        // of calm: bob 1.5-2.1 s, sway 2.4-3.2 s.
        let bob_s = 1.5 + (gkey & 0x3f) as f32 / 63.0 * 0.6;
        let sway_s = 2.4 + ((gkey >> 6) & 0x3f) as f32 / 63.0 * 0.8;
        let phase = ((gkey >> 12) & 0xff) as f32 / 255.0;
        let t = td_ms as f32 / 1000.0;
        let bob = ((t / bob_s + phase) * core::f32::consts::TAU).sin();
        let sway = ((t / sway_s + phase) * core::f32::consts::TAU).sin();
        Self {
            bob_px: (bob * amp).round() as i32,
            lead_px: (sway * amp * 0.8).round() as i32,
        }
    }
}

/// Push one 1:1 cat dest/source window `[top, bottom)` as one row-free
/// [`FreeSprite`] (UnderText, NEAREST). No band split or clipping here: the
/// renderer's `stamp_free_sprite`/scissor clips against the unclamped origin,
/// so §5.7's row-0 clip falls out of the signed `y`; clipping early would
/// sample different texels.
#[allow(
    clippy::too_many_arguments,
    reason = "one cat sprite: its dest box, its source window, and the living-cartoon pose"
)]
fn push_cat_free(
    free: &mut Vec<FreeSprite>,
    x: i32,
    w: i32,
    top: i32,
    bottom: i32,
    src_x: i32,
    src_y: i32,
    pose: CatIdlePose,
    fp: &mut u64,
) {
    let h = bottom - top;
    if w <= 0 || h <= 0 {
        return;
    }
    // The idle pose translates the dest rect only, so the sprite stays NEAREST
    // 1:1 (§5.3) with authored size and aspect. At the identity pose this is a
    // plain push.
    let s = FreeSprite {
        x: x + pose.lead_px,
        y: top + pose.bob_px,
        w: w as u16,
        h: h as u16,
        ax: src_x.max(0) as u16,
        ay: src_y.max(0) as u16,
        aw: w as u16, // NEAREST 1:1 (§5.3): bake at exact dest size
        ah: h as u16,
        tint: 0x00FF_FFFF, // §5.3: full-color bakes, tint stays neutral
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    };
    *fp = fold_free(*fp, &s);
    free.push(s);
}

/// The per-tick mutable context the feline arm threads through [`emit_cat`]:
/// the bakers plus the §F4.2 Kitty Log scratch.
struct CatTick<'a> {
    baker: &'a mut CatBaker,
    /// The animal roster's exact-size cache — [`emit_animal`] bakes here, then
    /// lands the texels in the shared atlas through `baker`'s `host_tile`
    /// door, so cats and animals ride one upload and one bake budget.
    animal_baker: &'a mut AnimalBaker,
    /// §F4.2: idents whose episode has not yet queued its one sighting (the
    /// pre-loop scratch; ≤ MAX_OCCURRENCES entries, linear scan is bounded).
    unlogged: &'a [u64],
    /// §F4.2: this tick's recorded sightings (resident, cap MAX_OCCURRENCES).
    sightings: &'a mut Vec<KittySighting>,
    /// This tick's positioned peek landings (resident, cap
    /// [`MAX_PEEK_CUES`]) — recorded at the sighting's once-per-episode
    /// edge, with the dest rect the sighting deliberately does not carry.
    peek_cues: &'a mut Vec<PeekCue>,
    /// The pixels the live cursor companion occupies, if one is up: a pixel
    /// box rather than a word test, since an ambient cat peeks two rows up or
    /// down and can land on the companion from another row. See
    /// [`CompanionOnGlass`] for the two rects.
    companion_px: CompanionFootprint,
}

/// Whether the cat rect `(x, w, top..bottom)` is stacked on the companion box
/// `a`, covered enough to read as a second cat in the same place.
/// `bottom`/`y1` are exclusive, matching `emit_peek`'s spans.
///
/// Judged by intersected area as a fraction of the cat's area: a cat peeking
/// up from a few rows below grazes the caret's row by ~5 %, while a genuine
/// stack covers most of the sprite. A third separates the two.
fn cat_is_stacked_on(a: (i32, i32, i32, i32), x: i32, w: i32, top: i32, bottom: i32) -> bool {
    let (x0, x1, y0, y1) = a;
    let iw = (x + w).min(x1) - x.max(x0);
    let ih = bottom.min(y1) - top.max(y0);
    if iw <= 0 || ih <= 0 {
        return false;
    }
    let cat_area = i64::from(w) * i64::from(bottom - top);
    if cat_area <= 0 {
        return false;
    }
    i64::from(iw) * i64::from(ih) * 3 >= cat_area
}

/// v3 §1.2: the episode-backed view of one feline occurrence's peek state,
/// copied out of the persist map before emission (direct-built occurrences
/// without an episode default to `start = appeared`, arm computed per frame).
#[derive(Clone, Copy)]
struct PeekView {
    /// The phase clock's origin (latched at birth in [`Episode::fresh`],
    /// regardless of focus). `None` is a defensive seam only — an unlatched
    /// clock draws nothing; episode-backed occurrences always carry `Some`.
    start: Option<Instant>,
    /// The graphic one-shot completed — zero quads forever.
    peek_done: bool,
    /// The frozen arm (`None` for direct-built occurrences).
    arm: Option<KittyShownAs>,
}

/// Emit one peeking cat: the genome wardrobe (magic, specials, accessories,
/// age) resolved into the shared [`emit_peek`] one-shot, then — once a body
/// has landed — the §F4.2 Kitty Log sighting and the pet's peek cue.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_ink; a carrier struct would rename the list, not shrink it"
)]
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
    // `feline.magic = false` pins every cat to the ordinary build (§10).
    let magic = if cfg.feline_magic {
        cat_magic(occ.genome.magic)
    } else {
        None
    };
    let special = if cfg.feline_magic {
        special_variant_v4(occ.genome.magic)
    } else {
        None
    };
    let variant = special.unwrap_or_else(|| cat_variant_v4(occ.genome.gkey));
    // Accessories overlay a plain head only (never a special).
    let accessory = if special.is_none() && cfg.feline_magic {
        accessory_variant_v4(occ.genome.magic)
    } else {
        None
    };
    let age = cat_age_v4(occ.genome.gkey);
    // The authored glyph path: a special (from the magic word) replaces the
    // head; otherwise the genome variant picks a head, and an overlay accessory
    // (bow/crown/bell) rides a plain head. Fills come from the genome and the
    // bounded local text/background palette.
    let g = cat_geometry(occ, geom, variant);
    let (coat, iris) = cat_fills_v4(occ.genome.gkey);
    let key = BakeKeyV4 {
        variant,
        accessory,
        coat,
        iris,
        colors: occ.cat_colors,
        w: g.w,
        h: g.hart,
        // Peeking word-cats keep their authored eyes (the blink/squint frames
        // belong to the animated cursor companion, not the roster cameo).
        eyes: EyesFrame::Open,
    };
    let Some((top, bottom)) = emit_peek(
        ctx,
        occ,
        cfg,
        geom,
        now,
        frame,
        peek,
        g,
        magic.is_some() || has_accessory(occ.genome.magic),
        age == CatAge::Kitten,
        |ctx| ctx.baker.get_v4(&key),
        free,
        fp,
        active_until,
    ) else {
        return;
    };
    // §F4.2 Kitty Log: a body landed, so queue this cat's once-per-episode
    // sighting (the post-loop pass flips `Episode::logged`). The only
    // per-identity trait logged is the overlay accessory (bow / crown).
    if ctx.unlogged.contains(&occ.ident) && ctx.sightings.len() < MAX_OCCURRENCES {
        // The pet's bat cue rides the same edge, with the dest rect that is
        // final only here. Cap-truncation drops the gag, never the log.
        if ctx.peek_cues.len() < MAX_PEEK_CUES {
            ctx.peek_cues.push(PeekCue {
                row: occ.row,
                col: occ.start_col,
                head_px: (i32::from(g.x), i32::from(g.x) + i32::from(g.w), top, bottom),
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

/// The species-head twin of [`emit_cat`]: the same one-shot peek without the
/// genome wardrobe (no magic, specials, accessories, age scale, kitten bounce
/// or paw fallback), and with no Kitty Log sighting or peek cue (the log and
/// the pet's bat are for cats). The tile rides the exact-size [`AnimalBaker`]
/// into the shared atlas through `host_tile`, so a deferred bake defers the
/// sprite one beat like a deferred cat.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_cat"
)]
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
    let g = animal_geometry(occ, geom, species);
    let key = AnimalBakeKey {
        species,
        colors: occ.cat_colors,
        w: g.w,
        h: g.hart,
    };
    // magical = false: the prepass's dwell derivation mirrors this (see
    // episode_prepass). Two doors, one budget (the pet's idiom): bake the
    // species head into its exact-size cache, then land the texels in the
    // shared atlas; a miss on either door defers the sprite one short beat.
    emit_peek(
        ctx,
        occ,
        cfg,
        geom,
        now,
        frame,
        peek,
        g,
        false,
        false,
        |ctx| {
            let rgba = ctx.animal_baker.tile(&key)?;
            ctx.baker.host_tile(key.host_id(), key.w, key.h, rgba)
        },
        free,
        fp,
        active_until,
    );
}

/// The one-shot peek cycle both rosters share (v3 §1.2):
///
/// ```text
/// Rise (450 ms, ease_out_back, genome overshoot)
/// → Dwell (genome 2200..3598 ms − mix(ident) % 300; +500 ms magic/accessory,
///          cap 3750; idle bob and sway)
/// → Descend (320 ms easeInOutCubic; optional 60 ms anticipation lift on
///            ~50% of genomes)
/// → Done (zero quads, forever for this episode)
/// ```
///
/// A pure function of `(now, genome, ident, PeekView)` plus the roster's
/// geometry `g` and `bake` door; `reduced_motion` shows the static settled
/// pose. Returns the landed dest span `(top, bottom)` when a body was pushed.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_ink; a carrier struct would rename the list, not shrink it"
)]
fn emit_peek<'a>(
    ctx: &mut CatTick<'a>,
    occ: &Occurrence,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    peek: PeekView,
    g: CatGeom,
    magical: bool,
    kitten: bool,
    bake: impl FnOnce(&mut CatTick<'a>) -> Option<CatTile>,
    free: &mut Vec<FreeSprite>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) -> Option<(i32, i32)> {
    let ch = i32::from(geom.cell_h);
    // §5.6 rest reveal: full Hart for every identity.
    let rest = i32::from(cat_rest_reveal(g.hart));
    // Entrance overshoot amplitude from a free v4 bit window (bits 15–16 sit
    // above the v4 art layout 0..=14, so they never fragment the bake key).
    let overshoot = 0.06 + genome::field(occ.genome.gkey, 15, 2) as f32 * 0.04;
    let amp = overshoot * if kitten { 1.3 } else { 1.0 };
    // v3 §6: the dwell range rides the graphic spec (class default
    // 2200..=3598; direct-built test occurrences fall back to it too).
    let dwell_range = occ.spec.graphic.map_or((2200, 3598), |g| g.dwell_ms);
    let dwell = peek_dwell_ms(occ.ident, occ.genome.gkey, magical, dwell_range);
    let antic = anticipation_ms(occ.genome.gkey);
    // `peek_total_ms`'s sum, from the parts already in hand.
    let total = CAT_RISE_MS + dwell + antic + CAT_DESCEND_MS;
    let dwell_end = CAT_RISE_MS + dwell;

    // reduced_motion pins the static settled pose; otherwise the clock runs
    // from the birth-latched phase start, regardless of focus.
    let (t, in_rise, in_dwell) = if cfg.reduced_motion {
        // A completed peek stays done under reduced motion (unfocused windows
        // are demoted to it). The clock latched at birth, so the pose shows
        // from the first presented frame and the flags never advance while
        // reduced. An unlatched clock (defensive) shows nothing.
        if peek.peek_done || peek.start.is_none() {
            return None;
        }
        (CAT_RISE_MS, false, true)
    } else {
        let ps = peek.start?; // defensive belt: an unlatched clock emits nothing
        if now < ps {
            // Defensive: a future clock origin emits nothing yet but keeps
            // frames coming until the rise begins.
            arm_until(active_until, ps + Duration::from_millis(total));
            *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
            return None;
        }
        let t = now.saturating_duration_since(ps).as_millis() as u64;
        if peek.peek_done || t >= total {
            return None; // Done: zero quads, forever for this episode
        }
        arm_until(active_until, ps + Duration::from_millis(total));
        (t, t < CAT_RISE_MS, t >= CAT_RISE_MS && t < dwell_end)
    };
    let td = t.saturating_sub(CAT_RISE_MS); // dwell-relative clock
    // Every peek frame changes bytes (rise, descend, idle pose), so the frame
    // term is folded throughout; `arm_until` already keeps the scheduler armed
    // for the peek, so this costs presents, not wakeups.
    if !cfg.reduced_motion {
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
    }
    let p = if in_rise {
        t as f32 / CAT_RISE_MS as f32
    } else {
        1.0
    };
    // Reveal + anticipation: Rise grows the reveal with ease-out-back
    // overshoot (reveal > rest lifts the dest); Dwell holds at rest; Descend
    // shrinks it back to 0 with easeInOutCubic after the optional 60 ms
    // anticipation lift.
    let mut antic_lift = 0i32;
    let reveal = if cfg.reduced_motion || in_dwell {
        rest
    } else if in_rise {
        (ease_out_back(p, amp) * rest as f32).round() as i32
    } else {
        let ta = t - dwell_end;
        if ta < antic {
            antic_lift = 2; // the pre-drop crouch reads as intent
            rest
        } else {
            let pd = (ta - antic) as f32 / CAT_DESCEND_MS as f32;
            ((1.0 - ease_in_out_cubic(pd)) * rest as f32).round() as i32
        }
    };
    if reveal <= 0 {
        return None; // pre-rise / post-descend edge frame: nothing visible
    }
    let Some(tile) = bake(ctx) else {
        // Bake deferred (≤ 2/frame, §5.5): emit nothing this frame; keep the
        // scheduler armed one short beat so the retry frame happens.
        arm_until(active_until, now + Duration::from_millis(50));
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        return None;
    };
    // §5.6 entrance kinematics, all bake-free: the dest bottom is pinned at
    // `word_row_top + chin`; the visible height grows to the rest reveal with
    // the top of the art entering first (ears → eyes → muzzle); overshoot
    // lifts the dest past rest; kittens land with a 2 px bounce early in the
    // dwell; the pre-descend anticipation lifts 2 px.
    let vh = reveal.min(rest);
    // §5.2 overshoot clamp: `lift ≤ 2·ch − (Hart − chin)`, so the head band
    // (rows r−2 and r−1) never lifts into row r−3.
    let lift = (reveal - rest)
        .max(0)
        .max(antic_lift)
        .min(2 * ch - (i32::from(g.hart) - i32::from(g.chin)));
    let in_bounce = kitten && !cfg.reduced_motion && in_dwell && td < CAT_BOUNCE_MS;
    let bounce = if in_bounce {
        let q = td as f32 / CAT_BOUNCE_MS as f32;
        (2.0 * (std::f32::consts::PI * q).sin()).round() as i32
    } else {
        0
    };
    let row_top = i32::from(occ.row) * ch;
    // Up anchors the chin slice at the word row's top edge and rises into the
    // two rows above; Down tucks it behind the bottom edge and slides into the
    // two rows below. The direction comes from [`cat_peek_plan`] at rescan,
    // not a viewport clamp, which would park the head on its own line.
    let (mut top, mut bottom) = if occ.cat_peek_down {
        let rest_top = row_top + ch - i32::from(g.chin);
        let top = rest_top + lift - bounce;
        (top, top + vh)
    } else {
        let rest_bottom = row_top + i32::from(g.chin);
        let bottom = rest_bottom - lift + bounce;
        (bottom - vh, bottom)
    };
    // Viewport clamps. `cat_peek_plan` only picks a side with room for the
    // body, so these bind on the overshoot/bounce lift alone; sliding by the
    // exact overhang preserves height, so the kinematics read identically.
    let grid_h = i32::from(geom.rows) * ch;
    if top < 0 {
        bottom -= top;
        top = 0;
    } else if bottom > grid_h {
        let over = bottom - grid_h;
        top -= over;
        bottom -= over;
    }
    // The art is only ever cut at the text line: Up pins `bottom` at the word
    // row and shows the art's top rows; Down feeds its bottom `vh` rows, so the
    // head lowers out from under the line. `rest` is the tile height
    // (`cat_rest_reveal` is the identity), `rest - vh` the hidden part.
    let src_y0 = if occ.cat_peek_down {
        i32::from(tile.ay) + (rest - vh).max(0)
    } else {
        i32::from(tile.ay)
    };
    // Yield to the companion in pixels, on the final dest rect `tick`'s
    // word-space veto cannot see. Returning equals not pushing (no sighting);
    // a cat merely beside the companion still draws.
    if ctx
        .companion_px
        .stacked_on(i32::from(g.x), i32::from(g.w), top, bottom)
    {
        return None;
    }
    let n_free = free.len();
    // One FreeSprite per cat over `[top, bottom)`. The idle pose applies only
    // in the dwell after the kitten's landing bounce: two vertical motions
    // together read as mush.
    let pose = if in_dwell && !in_bounce && !cfg.reduced_motion {
        CatIdlePose::breathing(td, occ.genome.gkey, geom.cell_h)
    } else {
        CatIdlePose::STILL
    };
    push_cat_free(
        free,
        i32::from(g.x),
        i32::from(g.w),
        top,
        bottom,
        i32::from(tile.ax),
        src_y0,
        pose,
        fp,
    );
    (free.len() > n_free).then_some((top, bottom))
}

/// The class base ink pair `(c0, c1)` (§4.2/§4.6), `0x00RRGGBB`. `None` = the
/// class carries no ink.
fn ink_pair(class: Class) -> Option<(u32, u32)> {
    match class {
        Class::Emphasis => Some((0x007C_C8FF, 0x00C8_9AFF)),
        Class::Profanity => Some((0x00FF_D447, 0x00FF_7CE5)),
        Class::Feline => Some((0x00F7_A8B8, 0x00FF_D9C2)),
        // Warm meadow pair. The class default is SelfGlow, so only a custom
        // TwoTone animal word reaches it (as its §6.5 coupling base).
        Class::Animal => Some((0x00FF_C98A, 0x00B9_E8A3)),
        Class::Canine => None,
    }
}

/// §4.2/§3.4 genome ink-pair nudge: the class base pair with each anchor's hue
/// rotated by its Gray-decoded 4-bit code (`-18°..=+18°` in 2.4° steps — a
/// bit-0 context flip moves one step). Profanity reads the nova layout window,
/// every other ink-bearing class the cat layout window (`genome::ink_pair_nudges`).
fn nudged_ink_pair(class: Class, gkey: u64) -> Option<(u32, u32)> {
    let (c0, c1) = ink_pair(class)?;
    let (n0, n1) = genome::ink_pair_nudges(class, gkey);
    Some((hue_nudge(c0, n0), hue_nudge(c1, n1)))
}

/// Feline (SelfGlow) envelope: two gentle pulses fading over the window and
/// settling to exactly 0, so the word returns to its own fg. Pure function of
/// elapsed ms; the window and amplitude come from the spec (defaults
/// [`FELINE_GLOW_MS`] / [`FELINE_GLOW_AMP`]).
fn feline_glow_env(t_ms: u64, window_ms: u64, amp: f32) -> f32 {
    if t_ms >= window_ms {
        return 0.0;
    }
    let p = t_ms as f32 / window_ms as f32;
    // Two smooth humps (0 at p = 0, 0.5, 1), decaying so the second is softer.
    let carrier = 0.5 - 0.5 * (4.0 * core::f32::consts::PI * p).cos();
    amp * carrier * (1.0 - p)
}

/// §4.2 specular lobe at gradient position `u` for sweep center `center`:
/// `smoothstep(1 − min(1, |u − center|/0.28))²` — 28% wide, 0 outside.
fn spec_lobe(u: f32, center: f32) -> f32 {
    let d = (u - center).abs() / 0.28;
    if d >= 1.0 {
        return 0.0;
    }
    let s = smoothstep(1.0 - d);
    s * s
}

/// Emit one occurrence's [`InkCell`]s for this frame (§4.2 visual model + §4.3
/// legibility guard), folding them into `fp` and extending `active_until` while
/// the sweep is live. No-op for occurrences with no captured ink (non-ink class,
/// ink disabled, or past the §4.4 truncation).
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators (fp / active_until) and the resident capture buffers; a carrier struct would rename the list, not shrink it"
)]
fn emit_ink(
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
    // v3 §6: the ink axis dispatches on the resolved spec colorway.
    let Some(ink_spec) = occ.spec.ink else {
        return;
    };
    match ink_spec.colorway {
        // SelfGlow words (feline) are not tinted: a glow pulse in the word's
        // own fg that ends at exactly the original fg (zero ink cells idle).
        // Born-done/born-settled words have the glow pre-spent: zero ink.
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
                return; // settled / reduced motion ⇒ the word is its own fg
            }
            // Keep the frame alive across the whole glow window (even at the
            // envelope zero-crossings) so the pulse is presented — a
            // paw-fallback word has no cat sprite to keep the scheduler
            // ticking. Self-terminating: the early-out above fires after it.
            let until = occ.appeared + Duration::from_millis(window);
            arm_until(active_until, until);
            let glow = feline_glow_env(t, window, amp);
            if glow <= 0.0 {
                return; // this frame sits at an envelope zero
            }
            for i in 0..occ.ink_cells as usize {
                let col = base_cols[occ.ink_base + i];
                let fg = rgb3_to_u32(base_fg[occ.ink_base + i]);
                // Glow toward a brightened version of the same color (never
                // pure white).
                let lit = mix_rgb(fg, 0x00FF_FFFF, lift);
                let cell = InkCell {
                    row: occ.row,
                    col,
                    color: u32_to_rgb3(mix_rgb(fg, lit, glow)),
                };
                *fp = fold_ink(*fp, &cell);
                *fp ^= frame.wrapping_mul(0x9E37_79B1); // animating: anti-skip
                ink.push(cell);
            }
            return;
        }
        // v3 §3.1: the rainbow colorway (its own emission arm below).
        Colorway::Rainbow {
            sat,
            val,
            span_deg,
            drift_ms,
        } => {
            emit_rainbow_ink(
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
    // §4.2 TwoTone: the class base pair, hue-nudged ±18° by the genome's
    // ink-pair Gray field — similar contexts shimmer in similar (not
    // identical) tints. Custom specs apply their colors raw (explicit config
    // is exact). The §6 machinery overrides the anchors (nova palette
    // endpoints / ember tint / coupling pulse) through `fx.pair`.
    let spec_pair = match ink_spec.colorway {
        Colorway::TwoTone { c0, c1 } if occ.custom => Some((c0, c1)),
        _ => nudged_ink_pair(occ.class, occ.genome.gkey),
    };
    let Some((c0, c1)) = fx.pair.or(spec_pair) else {
        return;
    };
    // v3 §6: `sweep_once = false` is the looping sweep (class defaults mirror
    // `cfg.ink_loop`; custom specs carry their own choice).
    let ink_loop = !ink_spec.sweep_once;
    let sweep = u64::from(cfg.ink_sweep_ms.max(1));
    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
    // reduced_motion: the static gradient from frame 0 — no sweep, and no
    // contribution to active_until (§4.5). §6.4 item 3: the word's own nova
    // window freezes the sweep at its settled gradient (fx.freeze), so the
    // nova's dip/flash pair is the only ink transition that second.
    let animating =
        !occ.inert && !fx.freeze && !cfg.reduced_motion && (ink_loop || t < sweep + INK_FADE_MS);
    let (center, glare) = if animating {
        // loop=true re-sweeps while visible: the center wraps per window, the
        // ramp-in applies once (first appearance), and there is no ramp-out.
        let phase = if ink_loop { t % sweep } else { t };
        let center = -0.28 + 1.56 * (phase as f32 / sweep as f32);
        let ramp_in = (t as f32 / INK_RAMP_IN_MS as f32).min(1.0);
        let ramp_out = if ink_loop || t < sweep {
            1.0
        } else {
            1.0 - smoothstep((t - sweep) as f32 / INK_FADE_MS as f32)
        };
        (center, 0.65 * ramp_in * ramp_out)
    } else {
        // Settled (or reduced motion): zero highlight ⇒ the emitted bytes are
        // exactly mix_rgb(base_fg, base(u), strength) — constant forever.
        (0.0, 0.0)
    };
    // §4.3 legibility guard on the word's mid-gradient ink, against the first
    // lead cell's bg.
    let mid_raw = mix_rgb(
        mix_rgb(c0, c1, 0.5),
        0x00FF_FFFF,
        glare * spec_lobe(0.5, center),
    );
    let strength = legible_strength(
        cfg.ink_strength,
        rgb3_to_u32(base_fg[occ.ink_base]),
        &[mid_raw],
        relative_luminance(rgb3_to_u32(occ.ink_bg)),
    );
    // Gradient parameter: u over the visual span (wide trailing halves count —
    // the §4.2 single normative, column-based definition).
    let span = f32::from(occ.end_col.saturating_sub(occ.start_col)).max(1.0);
    for i in 0..occ.ink_cells as usize {
        let col = base_cols[occ.ink_base + i];
        let u = f32::from(col.saturating_sub(occ.start_col)) / span;
        let base = mix_rgb(c0, c1, u);
        let lobe = glare * spec_lobe(u, center);
        let raw = if lobe > 0.0 {
            mix_rgb(base, 0x00FF_FFFF, lobe)
        } else {
            base
        };
        let mut color = mix_rgb(rgb3_to_u32(base_fg[occ.ink_base + i]), raw, strength);
        if fx.dim < 1.0 {
            // §6.1 Dip: the whole ink envelope dims ~35 % — the indrawn breath.
            color = dim_rgb(color, fx.dim);
        }
        let cell = InkCell {
            row: occ.row,
            col,
            color: u32_to_rgb3(color),
        };
        // The frame term keeps the fp changing on every animating frame (the
        // §6 dip ramp / coupling pulse included); folded mid-FNV-chain so it
        // diffuses per cell.
        *fp = fold_ink(*fp, &cell);
        if animating || fx.frame_live {
            *fp ^= frame.wrapping_mul(0x9E37_79B1);
        }
        ink.push(cell);
    }
    if animating {
        let until = if ink_loop {
            // A looping sweep never settles while visible; keep the scheduler's
            // is_active chain armed one window ahead.
            now + Duration::from_millis(sweep)
        } else {
            occ.appeared + Duration::from_millis(sweep + INK_FADE_MS)
        };
        arm_until(active_until, until);
    }
}

/// The §4.3/§3.1 legibility guard, per word per frame: pull the ink mix
/// `strength` toward the captured base fg `fg_first` (the theme's own legible
/// colour) in eighth-steps until every guard `sample`, mixed at that strength,
/// clears [`MIN_INK_CONTRAST`] against a background of luminance `lum_bg`.
/// Samples are tried in order and a pass stops at the first that fails.
fn legible_strength(ink_strength: f32, fg_first: u32, samples: &[u32], lum_bg: f32) -> f32 {
    let contrast = |c: u32| {
        let la = relative_luminance(c);
        (la.max(lum_bg) + 0.05) / (la.min(lum_bg) + 0.05)
    };
    let step = ink_strength.clamp(0.0, 1.0) / 8.0;
    let mut strength = ink_strength.clamp(0.0, 1.0);
    while strength > 0.0
        && samples
            .iter()
            .any(|&c| contrast(mix_rgb(fg_first, c, strength)) < MIN_INK_CONTRAST)
    {
        strength = (strength - step).max(0.0);
    }
    strength
}

/// Rainbow colorway emission (v3 §3.1): per-lead-cell hue
/// `H(cell) = base_hue(genome) + u · span_used + drift(t)` with
/// `span_used = min(span_deg, 100°·(lead_cells − 1))`: 300° on "fuck", a
/// two-step on 2-cell CJK, and a pure temporal drift for 1-cell words.
///
/// Byte-stable freeze: the drift phase is `360° · clamp(t, 0, drift_ms) /
/// drift_ms`, an exact full cycle, so the frozen phase equals the t = 0 phase
/// and a done-born rainbow is byte-identical to a settled one. The frame fp
/// fold is gated to `t < drift_ms` (+ live §6 fx), so settled frames
/// fingerprint stably. `reduced_motion` shows the frozen-phase rainbow.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
fn emit_rainbow_ink(
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
    let bg = rgb3_to_u32(occ.ink_bg);
    let lum_bg = relative_luminance(bg);
    // §3.1 theme-aware parameters: light backgrounds take deep candy tones
    // that clear 3.5:1 on white (bg luma > 0.5 ⇒ s = 1.0, v = 0.62).
    let (sat, val) = if lum_bg > 0.5 {
        (1.0, 0.62)
    } else {
        (sat, val)
    };
    let base_hue = rainbow_base_hue(occ.genome.gkey);
    // §3.1 span clamp: min(span_deg, 100°·(lead_cells − 1)).
    let span_used = span_deg.min(100.0 * f32::from(occ.ink_cells.saturating_sub(1)));
    let drift = u64::from(drift_ms.max(1));
    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
    let animating = !occ.inert && !cfg.reduced_motion && t < drift;
    // Exact full cycle: phase(drift) == phase(0) == 0 (mod 360).
    let phase = 360.0 * (t.min(drift) as f32 / drift as f32);
    let phase = if animating { phase } else { 0.0 };
    let hue_at = |u: f32| {
        hsv2rgb(
            (base_hue + u * span_used + phase).rem_euclid(360.0),
            sat,
            val,
        )
    };
    // §3.1 legibility guard: minimum contrast over u ∈ {0, ⅓, ⅔, 1} (a single
    // mid-gradient sample misses yellow/cyan washout). The guard hues do not
    // depend on `strength`, so they are computed once.
    let guard_hues = [
        hue_at(0.0),
        hue_at(1.0 / 3.0),
        hue_at(2.0 / 3.0),
        hue_at(1.0),
    ];
    let strength = legible_strength(
        cfg.ink_strength,
        rgb3_to_u32(base_fg[occ.ink_base]),
        &guard_hues,
        lum_bg,
    );
    // Per-lead-cell hue (§3.1): u indexes lead cells, not columns.
    let denom = f32::from(occ.ink_cells.saturating_sub(1)).max(1.0);
    for i in 0..occ.ink_cells as usize {
        let col = base_cols[occ.ink_base + i];
        let u = i as f32 / denom;
        let mut color = mix_rgb(rgb3_to_u32(base_fg[occ.ink_base + i]), hue_at(u), strength);
        // §3.2 supernova charge: toward white-hot on dark bg, toward
        // near-black on light (the eclipse's indrawn breath).
        if fx.lift > 0.0 {
            let target = if lum_bg > 0.5 {
                0x000A_0A10
            } else {
                0x00FF_FFFF
            };
            color = mix_rgb(color, target, 0.85 * fx.lift);
        }
        if fx.dim < 1.0 {
            color = dim_rgb(color, fx.dim);
        }
        let cell = InkCell {
            row: occ.row,
            col,
            color: u32_to_rgb3(color),
        };
        *fp = fold_ink(*fp, &cell);
        // §3.1 (normative): the frame fold is gated to t < drift_ms.
        if animating || fx.frame_live {
            *fp ^= frame.wrapping_mul(0x9E37_79B1);
        }
        ink.push(cell);
    }
    if animating {
        let until = occ.appeared + Duration::from_millis(drift);
        arm_until(active_until, until);
    }
}

/// Emit a small deterministic constellation around a freshly appeared
/// profanity rainbow. The stars share the ink episode's clock and terminate
/// after [`RAINBOW_SPARKLE_MS`]; no RNG, allocation, or additional scheduler
/// lifetime is introduced.
#[allow(
    clippy::too_many_arguments,
    reason = "pure tick-local rainbow decoration emitter"
)]
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
    let p = t as f32 / RAINBOW_SPARKLE_MS as f32;
    let envelope = (core::f32::consts::PI * p).sin().max(0.0);
    let width = occ.end_col.saturating_sub(occ.start_col).max(1);
    let base_hue = rainbow_base_hue(occ.genome.gkey);
    let lift = ((i32::from(geom.cell_h).max(8) / 3).min(127)) as i8;
    let count = 3usize.min(MAX_DECORATIONS.saturating_sub(out.len()));
    for i in 0..count {
        let u = i as f32 / (count.saturating_sub(1).max(1)) as f32;
        let phase =
            p * 4.0 * core::f32::consts::TAU + i as f32 * 2.094 + (occ.seed & 0xff) as f32 * 0.013;
        let shimmer = 0.62 + 0.38 * phase.sin().abs();
        let d = WordDecoration {
            row: occ.row,
            col: occ.start_col + (f32::from(width) * u).round() as u16,
            dx: (phase.cos() * 2.0).round() as i8,
            dy: -lift + i as i8,
            glyph: if i == 1 {
                DecoGlyph::Star5
            } else {
                DecoGlyph::Star4
            },
            blend: DecoBlend::Add,
            color: hsv2rgb((base_hue + 120.0 * i as f32 + 45.0 * p) % 360.0, 0.72, 1.0),
            alpha: scale_u8(cfg.intensity * envelope * shimmer * 0.82),
        };
        *fp = fold_deco(*fp, &d);
        out.push(d);
    }
    *fp ^= frame.wrapping_mul(0xD1B5_4A32_D192_ED03);
    let until = occ.appeared + Duration::from_millis(RAINBOW_SPARKLE_MS);
    arm_until(active_until, until);
}

/// Fold an ink cell's visible fields into the frame fingerprint (FNV-1a chain,
/// the `fold_deco` sibling).
pub(crate) fn fold_ink(mut h: u64, c: &InkCell) -> u64 {
    for x in [
        u64::from(c.row),
        u64::from(c.col),
        u64::from(c.color[0]),
        u64::from(c.color[1]),
        u64::from(c.color[2]),
    ] {
        h = fnv_mix_u64(h, x);
    }
    h
}

/// Scale a `0x00RRGGBB` colour's channels by `f` (the §6.1 dip envelope).
fn dim_rgb(c: u32, f: f32) -> u32 {
    let f = f.clamp(0.0, 1.0);
    let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * f).round() as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

pub(crate) fn rgb3_to_u32(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// Sample a bounded eight-cell neighborhood around a matched word. This runs
/// only when the damage epoch changes, never in the animation tick. The word's
/// first lead cell supplies the principal foreground/background; visible text
/// within four cells on either side supplies a harmonic accent. No allocation.
fn cat_color_context(cells: &[RenderCell], start: u16, end: u16) -> CatColorKey {
    if cells.is_empty() {
        return CatColorKey::default();
    }
    let start = usize::from(start).min(cells.len().saturating_sub(1));
    let end = usize::from(end).min(cells.len().saturating_sub(1));
    let Some(principal) = cells
        .get(start..=end)
        .and_then(|span| span.iter().find(|cell| !cell.wide))
        .copied()
    else {
        return CatColorKey::default();
    };

    let mut sum = [
        u32::from(principal.fg[0]),
        u32::from(principal.fg[1]),
        u32::from(principal.fg[2]),
    ];
    let mut count = 1u32;
    let lo = start.saturating_sub(4);
    let hi = end.saturating_add(4).min(cells.len().saturating_sub(1));
    for (i, cell) in cells[lo..=hi].iter().enumerate() {
        let col = lo + i;
        if (start..=end).contains(&col) || cell.wide || cell.ch.is_whitespace() || cell.ch == '\0' {
            continue;
        }
        for (dst, src) in sum.iter_mut().zip(cell.fg) {
            *dst += u32::from(src);
        }
        count += 1;
    }
    let surrounding = [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ];
    CatColorKey::from_rgb(
        rgb3_to_u32(principal.bg),
        rgb3_to_u32(principal.fg),
        rgb3_to_u32(surrounding),
    )
}

/// Geometry-aware shipping sampler. The prospective settled cat rectangle is
/// converted back to the exact grid cells it covers; every intersecting cell's
/// background and visible foreground contributes, with a hard cap for
/// degenerate metrics. Called only on a new episode's damage-driven rescan.
#[allow(
    clippy::too_many_arguments,
    reason = "the cold sampler receives one resolved occurrence span plus its immutable frame surface"
)]
fn cat_color_context_footprint(
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
        return CatColorKey::default();
    }
    let Some(principal) = cells
        .get(usize::from(row))
        .and_then(|line| {
            let lo = usize::from(start).min(line.len().saturating_sub(1));
            let hi = usize::from(end).min(line.len().saturating_sub(1));
            line.get(lo..=hi)
        })
        .and_then(|span| span.iter().find(|cell| !cell.wide))
    else {
        return CatColorKey::default();
    };
    let special = feline_magic
        .then(|| special_variant_v4(genome.magic))
        .flatten();
    let variant = special.unwrap_or_else(|| cat_variant_v4(genome.gkey));
    let g = cat_geometry_for(start, end, genome.gkey, geom, variant);
    let cw = i64::from(geom.cell_w);
    let ch = i64::from(geom.cell_h);
    let x0 = i64::from(g.x);
    let x1 = (x0 + i64::from(g.w)).min(i64::from(geom.cols) * cw);
    let y0 = (i64::from(row) * ch + i64::from(g.chin) - i64::from(g.hart)).max(0);
    let y1 = (i64::from(row) * ch + i64::from(g.chin)).min(i64::from(geom.rows) * ch);
    if x1 <= x0 || y1 <= y0 {
        return CatColorKey::default();
    }
    let c0 = usize::try_from(x0 / cw).unwrap_or(0);
    let c1 = usize::try_from((x1 - 1) / cw).unwrap_or(usize::MAX);
    let r0 = usize::try_from(y0 / ch).unwrap_or(0);
    let r1 = usize::try_from((y1 - 1) / ch).unwrap_or(usize::MAX);
    let mut bg_sum = [0u32; 3];
    let mut fg_sum = [0u32; 3];
    let mut sampled = 0u32;
    let mut visible = 0u32;
    let mut min_background_band = 3u8;
    let mut max_background_band = 0u8;
    let fallback_bg = u32_to_rgb3(default_bg);
    'rows: for line in (r0..=r1).map(|row| cells.get(row)) {
        for col in c0..=c1 {
            let cell = line.and_then(|line| line.get(col));
            let background = cell.map_or(fallback_bg, |cell| cell.bg);
            let band = CatColorKey::background_band(rgb3_to_u32(background));
            min_background_band = min_background_band.min(band);
            max_background_band = max_background_band.max(band);
            for (dst, src) in bg_sum.iter_mut().zip(background) {
                *dst += u32::from(src);
            }
            sampled += 1;
            if let Some(cell) = cell
                && !cell.wide
                && !cell.ch.is_whitespace()
                && cell.ch != '\0'
            {
                for (dst, src) in fg_sum.iter_mut().zip(cell.fg) {
                    *dst += u32::from(src);
                }
                visible += 1;
            }
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
        }
    }
    if sampled == 0 {
        return CatColorKey::default();
    }
    let background = bg_sum.map(|channel| (channel / sampled) as u8);
    let surrounding = if visible == 0 {
        principal.fg
    } else {
        fg_sum.map(|channel| (channel / visible) as u8)
    };
    CatColorKey::from_rgb_span(
        rgb3_to_u32(background),
        rgb3_to_u32(principal.fg),
        rgb3_to_u32(surrounding),
        min_background_band,
        max_background_band,
    )
}

/// Which way the two-band head enters relative to its trigger word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PeekDir {
    /// The classic peek: the head rises into the two rows above the word and
    /// looks over it, chin slice tucked behind the word row's top edge.
    Up,
    /// The mirrored peek: the head slides down out from under the word into
    /// the two rows below, chin tucked behind the word row's bottom edge. Used
    /// for a top-row word, where there is no room above.
    Down,
}

/// Occupancy ceiling for a candidate head band, as a fraction of sampled cells
/// carrying a visible glyph.
///
/// A ceiling, not a veto, so a `kitty` inside a TUI prompt box (`╭─╮ │ ╰─╯`)
/// still gets its cat: a frame's lines are not letters. A band with a letter
/// in it is no home at all ([`band_has_letter`]): cats are
/// [`FreeZ::UnderText`] sprites, but a light letter over white fur reads at
/// 1.4:1 (a fox's head over the line an Enter just printed).
const CAT_MAX_BAND_OCCUPANCY: f32 = 0.75;

/// Whether any cell of `[r0, r1] × [c0, c1]` holds a letter: a visible glyph
/// that is not a box-drawing or block element (a frame's line, which a head
/// may sit behind). A head never lands on a letter, so a band holding one is
/// not a side it can enter from.
fn band_has_letter(cells: &[Vec<RenderCell>], r0: usize, r1: usize, c0: usize, c1: usize) -> bool {
    (r0..=r1).any(|r| {
        cells.get(r).is_some_and(|line| {
            (c0..=c1).any(|c| {
                line.get(c).is_some_and(|cell| {
                    cell.wide
                        || (!cell.ch.is_whitespace()
                            && cell.ch != '\0'
                            && !('\u{2500}'..='\u{259F}').contains(&cell.ch))
                })
            })
        })
    })
}

/// Fraction of the sampled cells in `[r0, r1] × [c0, c1]` carrying a visible
/// glyph, or `None` when the span is degenerate or exceeds the 64-cell
/// cold-path sampling bound.
fn band_occupancy(
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
    let (mut sampled, mut occupied) = (0u32, 0u32);
    for r in r0..=r1 {
        for c in c0..=c1 {
            if skip_cols.as_ref().is_some_and(|s| s.contains(&c)) {
                continue;
            }
            sampled += 1;
            if cells
                .get(r)
                .and_then(|line| line.get(c))
                .is_some_and(|cell| cell.wide || (!cell.ch.is_whitespace() && cell.ch != '\0'))
            {
                occupied += 1;
            }
        }
    }
    (sampled > 0).then(|| occupied as f32 / sampled as f32)
}

/// Resolve the head's entry direction for one prospective cat, or `None` when
/// neither side can host it. A band holding a letter is unavailable
/// ([`band_has_letter`]), as is one off the viewport (so the top row peeks
/// down); the rest are scored by [`band_occupancy`] (the word's own row
/// folded in) and the clearer wins, ties to [`PeekDir::Up`]; both over
/// [`CAT_MAX_BAND_OCCUPANCY`] is a wall and no cat draws.
fn cat_peek_plan(
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
    let special = feline_magic
        .then(|| special_variant_v4(genome.magic))
        .flatten();
    let variant = special.unwrap_or_else(|| cat_variant_v4(genome.gkey));
    let g = cat_geometry_for(start, end, genome.gkey, geom, variant);
    let cw = i64::from(geom.cell_w);
    let ch = i64::from(geom.cell_h);
    let x0 = i64::from(g.x);
    let x1 = (x0 + i64::from(g.w)).min(i64::from(geom.cols) * cw);
    if x1 <= x0 {
        return None;
    }
    let c0 = usize::try_from(x0 / cw).unwrap_or(0);
    let c1 = usize::try_from((x1 - 1) / cw).unwrap_or(usize::MAX);
    // Rows the head body would occupy on each side. `body` is the head height
    // above the chin slice — the part that lands on neighbouring rows.
    let body = (i64::from(g.hart) - i64::from(g.chin)).max(0);
    let band_rows = usize::try_from((body + ch - 1) / ch).unwrap_or(0);
    if band_rows == 0 {
        // A head with no body outside the word row disturbs nothing either way.
        return Some(PeekDir::Up);
    }
    let trigger = usize::from(row);
    let rows = usize::from(geom.rows);
    // The word's own row, excluding the word itself: shared by both scores.
    let own = band_occupancy(
        cells,
        trigger,
        trigger,
        c0,
        c1,
        Some(usize::from(start)..=usize::from(end)),
    );
    let side = |lo: usize, hi: usize| -> Option<f32> {
        let band = band_occupancy(cells, lo, hi, c0, c1, None)?;
        // Fold the trigger row in at half weight: it is one row of a
        // three-row silhouette and the chin slice is mostly behind the word.
        Some(own.map_or(band, |o| (band * 2.0 + o) / 3.0))
    };
    let up = (trigger >= band_rows
        && !band_has_letter(cells, trigger - band_rows, trigger - 1, c0, c1))
    .then(|| side(trigger - band_rows, trigger - 1))
    .flatten();
    let down = (trigger + band_rows < rows
        && !band_has_letter(cells, trigger + 1, trigger + band_rows, c0, c1))
    .then(|| side(trigger + 1, trigger + band_rows))
    .flatten();
    match (up, down) {
        (Some(u), Some(d)) => {
            let (score, dir) = if d < u {
                (d, PeekDir::Down)
            } else {
                (u, PeekDir::Up)
            };
            (score <= CAT_MAX_BAND_OCCUPANCY).then_some(dir)
        }
        // Only one side exists (top/bottom row). Take it if it is habitable;
        // the ceiling still rejects a solid wall.
        (Some(u), None) => (u <= CAT_MAX_BAND_OCCUPANCY).then_some(PeekDir::Up),
        (None, Some(d)) => (d <= CAT_MAX_BAND_OCCUPANCY).then_some(PeekDir::Down),
        (None, None) => None,
    }
}

pub(crate) fn u32_to_rgb3(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// WCAG contrast ratio (≥ 1) between two `0x00RRGGBB` colours — the tests'
/// oracle for [`legible_strength`].
#[cfg(test)]
fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Seed an occurrence from its column + class + scanner-canonical form hash, so
/// case-only redraws keep identity (row excluded → scroll-stable). [`FormId`]
/// equality independently guards every adoption, so this hash is never treated
/// as proof that two different lexical surfaces are equal.
fn seed_of(start_col: u16, class: Class, form_hash: u64) -> u64 {
    let mut h = FNV64_OFFSET;
    for x in [u64::from(start_col), class_tag(class), form_hash] {
        h = fnv_mix_u64(h, x);
    }
    h
}

fn class_tag(c: Class) -> u64 {
    match c {
        Class::Profanity => 1,
        Class::Feline => 2,
        // 3 is retired (a deleted class): tags are seed inputs, so no class is
        // renumbered.
        Class::Emphasis => 4,
        Class::Canine => 5,
        Class::Animal => 6,
    }
}

fn glyph_id(g: DecoGlyph) -> u64 {
    match g {
        DecoGlyph::Star4 => 0,
        DecoGlyph::Star5 => 1,
        DecoGlyph::Dot => 2,
        DecoGlyph::Plus => 3,
        DecoGlyph::Paw => 4,
        // 5 is retired: ids are fingerprint inputs.
        DecoGlyph::RingArc => 6,
        DecoGlyph::Shade => 7,
    }
}

/// Fold a decoration's visible fields into the frame fingerprint.
fn fold_deco(mut h: u64, d: &WordDecoration) -> u64 {
    for x in [
        u64::from(d.row),
        u64::from(d.col),
        u64::from(d.dx as u8),
        u64::from(d.dy as u8),
        u64::from(d.color),
        u64::from(d.alpha),
        glyph_id(d.glyph),
        u64::from(matches!(d.blend, DecoBlend::Add)),
    ] {
        h = fnv_mix_u64(h, x);
    }
    h
}

/// Fold one [`FreeSprite`] into the frame fingerprint, every field: `z` and
/// `sampler` too (a same-rect Under→Over flip must change `deco_fp` or the
/// early-out would swallow it), and the signed `x`/`y` as i32 bit patterns.
/// The paired `free_atlas` version is folded once per tick.
fn fold_free(mut h: u64, s: &FreeSprite) -> u64 {
    for x in [
        u64::from(s.x as u32), // i32 bit pattern (signed off-grid origins)
        u64::from(s.y as u32),
        u64::from(s.w),
        u64::from(s.h),
        u64::from(s.ax),
        u64::from(s.ay),
        u64::from(s.aw),
        u64::from(s.ah),
        u64::from(s.tint),
        u64::from(s.alpha),
        u64::from(s.flip_x),
        s.z as u64,
        s.sampler as u64,
    ] {
        h = fnv_mix_u64(h, x);
    }
    h
}

/// Fold a supernova light quad's visible fields into the frame fingerprint
/// (the `fold_free`/`fold_deco` sibling for the §6 `nova_add` stream).
fn fold_glow(mut h: u64, q: &GlowQuad) -> u64 {
    for x in [
        u64::from(q.row),
        u64::from(q.x),
        u64::from(q.y),
        u64::from(q.w),
        u64::from(q.h),
        u64::from(q.color),
    ] {
        h = fnv_mix_u64(h, x);
    }
    h
}

/// Pick a sparkle colour: a palette entry if configured, else a hue rotation.
fn pick_color(cfg: &DecoConfig, s: u64) -> u32 {
    if cfg.palette.is_empty() {
        hsv2rgb((s % 360) as f32, 0.85, 1.0)
    } else {
        cfg.palette[(s >> 23) as usize % cfg.palette.len()]
    }
}

/// The sparkle-words twinkle envelope: the shared pulse law
/// ([`crate::effect_util::twinkle_env`]), phase-offset per spark.
///
/// Driven by the occurrence's age from the injected `now`, not a frame
/// counter, so it runs at [`crate::effect_util::TWINKLE_FLASH_HZ`] on every
/// panel, inside the WCAG 2.3.1 flash bound
/// (`effect_util`'s `twinkle_law_stays_under_the_photosensitivity_bound`).
fn twinkle(s: u64, age_s: f32) -> f32 {
    let phase = (s & 0xff) as f32 / 255.0 * std::f32::consts::TAU;
    crate::effect_util::twinkle_env(age_s, phase)
}

/// A signed sub-cell jitter in `[-j, j]` derived from `s`.
fn jitter(j: i8, s: u64) -> i8 {
    if j <= 0 {
        return 0;
    }
    let span = 2 * i64::from(j) + 1;
    (((s % span as u64) as i64) - i64::from(j)) as i8
}

fn scale_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The yield collides against the flying head's real sprite (from
    /// [`WordDecorations::kitty_cursor_footprint`]), ~1.75 cells right of and
    /// ~0.4 rows above the caret: a word-cat drawn on the head yields.
    #[test]
    fn an_ambient_cat_on_the_flying_head_yields_to_it() {
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 6,
            cols: 20,
        };
        let cell = (3u16, 0u16);
        let wd = WordDecorations::default();
        let head = wd
            .kitty_cursor_footprint(still_layout(geom, cell))
            .expect("the head is placeable on a 20-column grid");
        let head_rect = (
            head.x,
            head.x + i32::from(head.w),
            head.y,
            head.y + i32::from(head.h),
        );
        // Precondition: the head is off the caret cell.
        assert!(
            head.x >= i32::from(geom.cell_w),
            "the head leads the caret column by more than a whole cell (x = {})",
            head.x
        );

        // An ambient cat drawn exactly where the head is drawn.
        let (cx, cw) = (head.x, i32::from(head.w));
        let (ctop, cbot) = (head.y, head.y + i32::from(head.h));

        let fixed = CompanionOnGlass::at_head(cell, head_rect).footprint(geom);
        assert!(
            fixed.stacked_on(cx, cw, ctop, cbot),
            "a cat drawn on the flying head must yield to it"
        );

        // Negative control: a caret-band-only model lets that same cat draw.
        let caret_band_only = CompanionOnGlass::at_cell(cell).footprint(geom);
        assert!(
            !caret_band_only.stacked_on(cx, cw, ctop, cbot),
            "the caret band alone cannot see the head — this WAS the bug"
        );

        // The caret band still catches a cat on the glyph being typed.
        assert!(
            fixed.stacked_on(2, 20, 58, 82),
            "a cat on the caret cell still yields"
        );
        assert!(
            caret_band_only.stacked_on(2, 20, 58, 82),
            "non-vacuous: the caret band is what catches the caret cat"
        );

        // ...and no greedier: the away cat still draws against both rects.
        assert!(
            !fixed.stacked_on(8, 33, 76, 104),
            "the away cat still peeks beside the companion"
        );

        // The resident pet guards its body and nothing else: it stands where
        // its brain walked it and has no claim on the caret.
        let pet = CompanionOnGlass::at_body(Some(cell), (100, 128, 40, 74)).footprint(geom);
        assert!(
            pet.stacked_on(100, 28, 40, 74),
            "a cat on the pet's body yields to it"
        );
        assert!(
            !pet.stacked_on(2, 20, 58, 82),
            "the pet claims no caret band"
        );
    }

    /// THE PAPER LAW FOR THE PET'S ♪ AND ♥ (the cheer's motes,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3): on white paper
    /// (Aterm Day's ground) and on cream (Linen's) each ♪/♥ of a cheer is
    /// drawn at 4.5:1 or more against the page with its hue kept; on a dark
    /// ground each keeps its per-kind tint, untouched. NEGATIVE CONTROL (run
    /// by hand, files restored): the page ignored (`pet_mote_on_page`
    /// returning the tint) draws the gold ♪ at under 2:1 on white.
    #[test]
    fn a_cheers_motes_on_paper_keep_their_hue_at_the_marks_contrast() {
        use crate::kitty_pet::{PetMoteKind, PetMoteSprite};
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 12,
            cols: 40,
        };
        let mut pet =
            crate::kitty_pet::PetBrain::default().tick_static_capture(crate::kitty_pet::PetSense {
                caret_drawn: true,
                now: Instant::now(),
                caret: Some((6, 10)),
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell_w,
                cell_h: geom.cell_h,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
                wrapped: false,
            });
        assert!(pet.alpha > 0, "fixture: the pet is on glass");
        for (k, kind) in [PetMoteKind::Note, PetMoteKind::Heart, PetMoteKind::Note]
            .into_iter()
            .enumerate()
        {
            pet.motes[k] = Some(PetMoteSprite {
                kind,
                col: 14.0 + k as f32,
                row: 4.0,
                scale: 1.0,
                rot: 0.4,
                alpha: 255,
                rgb: None,
            });
        }
        let tints = |page: Option<u32>| {
            let mut wd = WordDecorations::default();
            let mut free = Vec::new();
            // A frame's bake budget takes one new tile family at a time: the
            // third frame draws every mote from warm tiles.
            for _ in 0..3 {
                free.clear();
                wd.begin_host_frame();
                let _ = wd.pet_cursor(
                    PetCursorFrame {
                        geom,
                        colors: CatColorKey::default(),
                        coat: 0,
                        iris: 0,
                        pet,
                        page,
                    },
                    &mut free,
                );
            }
            // The body is the one white-tinted sprite; the rest are motes.
            free.iter()
                .filter(|s| s.tint != 0x00FF_FFFF)
                .map(|s| s.tint)
                .collect::<Vec<_>>()
        };
        let contrast = |a: u32, b: u32| {
            let (ya, yb) = (relative_luminance(a), relative_luminance(b));
            (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
        };
        let hue = |rgb: u32| crate::color_math::rgb2hsv(rgb).0;
        let dark = tints(None);
        assert_eq!(dark.len(), 3, "fixture: the three motes drew: {dark:06x?}");
        for (k, &tint) in dark.iter().enumerate() {
            let kind = pet.motes[k].map(|m| m.kind).unwrap();
            let want = WordDecorations::pet_mote_tint(kind, CatColorKey::default());
            assert_eq!(tint, want, "a dark ground keeps the per-kind tint");
        }
        for (name, bg) in [("white", 0x00F8_FAFDu32), ("cream", 0x00F9_F3E6)] {
            let paper = tints(Some(bg));
            assert_eq!(paper.len(), 3, "{name}");
            for (&ink, &tint) in paper.iter().zip(&dark) {
                assert!(
                    contrast(ink, bg) >= 4.5 - 1e-3,
                    "{name}: {ink:06x} on {bg:06x} reads at {:.2}:1",
                    contrast(ink, bg)
                );
                let d = (hue(ink) - hue(tint)).rem_euclid(360.0);
                assert!(
                    d.min(360.0 - d) <= 12.0,
                    "{name}: {ink:06x} keeps {tint:06x}'s hue ({} vs {})",
                    hue(ink),
                    hue(tint)
                );
            }
        }
    }

    /// The one-body law: the first companion body to land owns the host frame
    /// and the second is refused.
    #[test]
    fn one_host_frame_admits_exactly_one_companion_body() {
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 6,
            cols: 20,
        };
        let now = Instant::now();
        let cell = (3u16, 4u16);
        let head_frame = |wd: &mut WordDecorations, free: &mut Vec<FreeSprite>| {
            let layout = still_layout(geom, cell);
            let placement = wd
                .resolve_kitty_cursor_placement(layout, settled_at(now))
                .expect("placeable companion");
            wd.kitty_cursor_at_placement(still_frame(geom, cell), placement, free)
        };

        // Positive check first: in its own frame the head draws, so the refusal
        // below is real.
        let mut wd = WordDecorations::default();
        wd.begin_host_frame();
        let mut alone = Vec::new();
        assert!(
            head_frame(&mut wd, &mut alone).is_some() && !alone.is_empty(),
            "non-vacuous: the flying head emits a body in a frame of its own"
        );

        // Now a frame the pet has already claimed. Same call, no body.
        wd.begin_host_frame();
        let mut free = Vec::new();
        let pet =
            crate::kitty_pet::PetBrain::default().tick_static_capture(crate::kitty_pet::PetSense {
                caret_drawn: true,
                now,
                caret: Some(cell),
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell_w,
                cell_h: geom.cell_h,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
                wrapped: false,
            });
        assert!(pet.alpha > 0, "non-vacuous: the pet is on glass");
        assert!(
            wd.pet_cursor(
                PetCursorFrame {
                    geom,
                    colors: CatColorKey::default(),
                    coat: 0,
                    iris: 0,
                    pet,
                    page: None,
                },
                &mut free,
            )
            .is_some(),
            "the pet claims the frame"
        );
        let after_pet = free.len();
        assert!(after_pet > 0, "non-vacuous: the pet body landed");
        assert!(
            head_frame(&mut wd, &mut free).is_none(),
            "the flying head may not add a second body to the pet's frame"
        );
        assert_eq!(
            free.len(),
            after_pet,
            "not one sprite — notes included — from the refused companion"
        );

        // The door reopens with the next frame.
        wd.begin_host_frame();
        let mut next = Vec::new();
        assert!(
            head_frame(&mut wd, &mut next).is_some(),
            "the next frame admits a companion again"
        );
    }

    fn cfg() -> DecoConfig {
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
            // The sparkle batteries pin the sparkle style; the nova battery
            // sets `Nova` itself.
            profanity_style: ProfanityStyle::Sparkle,
            profanity_magic: true,
            supernova_chance: 30,
            spec_table: SpecTable::default(),
            palette: vec![0x00FF_D447, 0x00FF_7CE5],
            density: 3,
            anim_ms: 2000,
            jitter: 2,
            intensity: 0.85,
            glyphs: vec![DecoGlyph::Star4, DecoGlyph::Dot],
            ignore: HashSet::new(),
        }
    }

    fn occ(class: Class, now: Instant) -> Occurrence {
        Occurrence {
            row: 0,
            start_col: 2,
            end_col: 5,
            class,
            langs: LangSet::EMPTY,
            form_id: FormId::UNKNOWN,
            seed: 0xDEAD_BEEF,
            ident: mix(0xDEAD_BEEF),
            appeared: now,
            genome: Genome { gkey: 0, magic: 0 },
            ink_base: 0,
            ink_cells: 0,
            ink_bg: [0; 3],
            cat_colors: CatColorKey::default(),
            cat_text_clear: true,
            cat_peek_down: false,
            dec_line: false,
            inert: false,
            // Direct-built occurrences resolve their spec from the test cfg,
            // as the rescan would.
            spec: class_default_spec(class, &cfg()),
            custom: false,
            species: None,
        }
    }

    /// `tick` with a throwaway ink + sprite scratch and no geometry (the
    /// all-zero [`EffectGeom`] trips the cat eligibility floor, so feline words
    /// emit ink but no graphic), for the pre-cat decoration tests.
    fn tick_deco(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        out: &mut Vec<WordDecoration>,
    ) -> u64 {
        let mut ink = Vec::new();
        let mut fr = Vec::new();
        let mut nova = Vec::new();
        wd.tick(
            now,
            cfg,
            EffectGeom::default(),
            None,
            None,
            true,
            out,
            &mut ink,
            &mut fr,
            &mut nova,
        )
    }

    #[test]
    fn profanity_sparkles_then_self_terminates() {
        let now = Instant::now();
        let mut wd = WordDecorations {
            occ: vec![occ(Class::Profanity, now)],
            cols: 80,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let c = cfg();
        let mut out = Vec::new();

        let fp1 = tick_deco(&mut wd, now, &c, &mut out);
        assert_eq!(
            out.len(),
            c.density as usize,
            "sparks == density while animating"
        );
        assert!(out.iter().all(|d| matches!(d.blend, DecoBlend::Add)));
        let fp2 = tick_deco(&mut wd, now, &c, &mut out);
        assert_ne!(fp1, fp2, "animating sparkle fingerprint changes each frame");
        assert!(wd.is_active(now));

        // v3 §1.2 graphics decay: the residual spark fades after the animation
        // window, then self-terminates to zero decos.
        let fading = now + Duration::from_millis(c.anim_ms + 50);
        tick_deco(&mut wd, fading, &c, &mut out);
        assert_eq!(out.len(), 1, "the residual spark is fading");
        let alpha_early = out[0].alpha;
        assert!(wd.is_active(fading), "the fade is still animating");
        let later = now + Duration::from_millis(c.anim_ms + 1500);
        tick_deco(&mut wd, later, &c, &mut out);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].alpha < alpha_early,
            "the residual fades monotonically"
        );
        // Past the ≤ 2 s fade: zero decos, stable fp, disarmed forever.
        let done = now + Duration::from_millis(c.anim_ms + 2100);
        let fpa = tick_deco(&mut wd, done, &c, &mut out);
        assert!(out.is_empty(), "the residual self-terminates to zero decos");
        let fpb = tick_deco(&mut wd, done + Duration::from_millis(16), &c, &mut out);
        assert_eq!(fpa, fpb, "post-fade fp is stable");
        assert!(!wd.is_active(done), "must self-terminate after the fade");
    }

    #[test]
    fn reduced_motion_forces_static() {
        let now = Instant::now();
        let mut wd = WordDecorations {
            occ: vec![occ(Class::Profanity, now)],
            cols: 80,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let mut c = cfg();
        c.reduced_motion = true;
        let mut out = Vec::new();
        tick_deco(&mut wd, now, &c, &mut out);
        assert_eq!(out.len(), 1, "reduced motion = single steady spark");
        assert!(!wd.is_active(now));
    }

    /// `fix` and its near-misses never take or inherit the profanity rainbow,
    /// typed incrementally or overwriting a live `fuck` in place.
    #[test]
    fn fix_never_triggers_or_inherits_profanity_rainbow() {
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let now = Instant::now();
        let mut term = Terminal::new(4, 48);
        let mut wd = WordDecorations::default();

        for (i, byte) in b"fix".iter().copied().enumerate() {
            term.process(&[byte]);
            wd.rescan(&term, 4, 48, &lex, &c, i as u64 + 1, now);
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Profanity),
                "incremental prefix {:?} classified as profanity",
                &b"fix"[..=i]
            );
        }

        for token in ["FIX", "fix!", "prefix", "suffix", "fixes", "ﬁx"] {
            let mut candidate = Terminal::new(2, 48);
            candidate.process(token.as_bytes());
            let mut scan = WordDecorations::default();
            scan.rescan(&candidate, 2, 48, &lex, &c, 1, now);
            assert!(
                scan.occ.iter().all(|o| o.class != Class::Profanity),
                "near-miss {token:?} classified as profanity"
            );
        }

        // Positive control: the real token does start a profanity episode.
        term.process(b"\rfuck\x1b[K");
        wd.rescan(&term, 4, 48, &lex, &c, 10, now);
        assert!(wd.occ.iter().any(|o| o.class == Class::Profanity));

        // Same-position replacement evicts the visible occurrence at once; grace
        // may keep identity metadata but never emits for absent text.
        term.process(b"\rfix\x1b[K");
        wd.rescan(&term, 4, 48, &lex, &c, 11, now + Duration::from_millis(16));
        assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
        let (mut deco, mut ink, mut free, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let fp = wd.tick(
            now + Duration::from_millis(32),
            &c,
            EffectGeom::default(),
            None,
            None,
            true,
            &mut deco,
            &mut ink,
            &mut free,
            &mut nova,
        );
        assert_eq!(
            fp, 0,
            "replacing fuck with fix leaves no animated fingerprint"
        );
        assert!(deco.is_empty() && ink.is_empty() && free.is_empty() && nova.is_empty());
    }

    /// Ambiguous forms at the live caret (`fut`, `futu` while typing `future`)
    /// wait for a delimiter with `ro` or every language on; `fuck` is immediate.
    /// Both scan entry points honour the live cursor.
    #[test]
    fn ambiguous_future_prefix_waits_for_delimiter_in_ro_and_all() {
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let now = Instant::now();

        for language in ["ro", "all"] {
            let lex = Lexicon::with_languages(&[language]);
            let mut term = Terminal::new(2, 48);
            let mut wd = WordDecorations::default();

            for (i, byte) in b"future".iter().copied().enumerate() {
                term.process(&[byte]);
                wd.rescan(&term, 2, 48, &lex, &c, i as u64 + 1, now);
                assert!(
                    wd.occ.iter().all(|o| o.class != Class::Profanity),
                    "{language} incremental future prefix {:?} classified as profanity",
                    &b"future"[..=i]
                );
            }

            for token in ["fu", "fix", "future"] {
                let mut candidate = Terminal::new(2, 48);
                candidate.process(token.as_bytes());
                let mut scan = WordDecorations::default();
                scan.rescan(&candidate, 2, 48, &lex, &c, 20, now);
                assert!(
                    scan.occ.iter().all(|o| o.class != Class::Profanity),
                    "{language} exact-token path classified {token:?} as profanity"
                );
            }

            for token in ["fut ", "futu "] {
                let mut settled = Terminal::new(2, 48);
                settled.process(token.as_bytes());
                let mut scan = WordDecorations::default();
                scan.rescan(&settled, 2, 48, &lex, &c, 21, now);
                assert!(
                    scan.occ.iter().any(|o| o.class == Class::Profanity),
                    "{language} delimiter must settle exact Romanian form {token:?}"
                );
            }

            let mut profanity = Terminal::new(2, 48);
            profanity.process(b"fuck");
            let mut scan = WordDecorations::default();
            scan.rescan(&profanity, 2, 48, &lex, &c, 30, now);
            assert!(
                scan.occ.iter().any(|o| o.class == Class::Profanity),
                "{language} exact fuck must retain the immediate profanity effect"
            );

            // The shipping renderer scans an already-resolved cell snapshot,
            // not the `Terminal`: its explicit cursor plumbing must defer the
            // same provisional `fut` and settle it on the delimiter, or the
            // live window and introspection captures drift apart.
            let geom = EffectGeom {
                cell_w: 8,
                cell_h: 16,
                rows: 2,
                cols: 48,
            };
            let mut term = Terminal::new(2, 48);
            let mut wd = WordDecorations::default();
            let mut snap = aterm_core::render::RenderInput::default();
            for (epoch, (bytes, settled)) in [(b"fut".as_slice(), false), (b" ".as_slice(), true)]
                .into_iter()
                .enumerate()
            {
                term.process(bytes);
                term.cell_frame_into(&mut snap, 2, 48);
                let cursor = term.cursor();
                wd.rescan_from_cells_with_geom_at_cursor(
                    &snap.cells,
                    &snap.line_sizes,
                    2,
                    48,
                    &lex,
                    &c,
                    epoch as u64 + 1,
                    now,
                    geom,
                    snap.default_bg,
                    Some((cursor.row, cursor.col)),
                );
                assert_eq!(
                    wd.occ.iter().any(|o| o.class == Class::Profanity),
                    settled,
                    "{language} snapshot path after {bytes:?}"
                );
            }
        }
    }

    /// `fuc` is ordinary; the fourth character is the first that may create
    /// the episode and its typed cue.
    #[test]
    fn fuc_is_ordinary_and_only_complete_fuck_activates() {
        let lex = Lexicon::with_languages(&["all"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        // Pins completion, not detonation: a won supernova roll adds a cue.
        c.supernova_chance = 0;
        let t0 = Instant::now();
        let model = aterm_models_misc::exact_profanity_completion_model();
        let mut state = model.init_state();
        let mut term = Terminal::new(2, 48);
        let mut wd = WordDecorations::default();

        for (epoch, (byte, action)) in [(b'f', "TypeF"), (b'u', "TypeU"), (b'c', "TypeC")]
            .into_iter()
            .enumerate()
        {
            term.process(&[byte]);
            assert!(model.fire(action, &mut state));
            wd.rescan(
                &term,
                2,
                48,
                &lex,
                &c,
                epoch as u64 + 1,
                t0 + Duration::from_millis(epoch as u64 * 8),
            );
            assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
            assert_eq!(state["active"], 0);
        }

        let (mut deco, mut ink, mut free, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        wd.tick(
            t0 + Duration::from_millis(16),
            &c,
            EffectGeom::default(),
            None,
            None,
            true,
            &mut deco,
            &mut ink,
            &mut free,
            &mut nova,
        );
        let prefix_cues = wd.drain_curse_cues().count();
        assert_eq!(prefix_cues, 0, "`fuc` emitted a curse cue");
        assert_eq!(state["episode"], prefix_cues as i64);

        term.process(b"k");
        assert!(model.fire("TypeK", &mut state));
        let completed_at = t0 + Duration::from_millis(24);
        // The `k` is a committed key: the bonk's typed witness needs it as well
        // as the caret position, since output parks the caret there too.
        wd.note_typed_edit(completed_at, None);
        wd.rescan(&term, 2, 48, &lex, &c, 4, completed_at);
        assert!(wd.occ.iter().any(|o| o.class == Class::Profanity));
        assert_eq!(state["active"], 1);
        wd.tick(
            completed_at,
            &c,
            EffectGeom::default(),
            None,
            None,
            true,
            &mut deco,
            &mut ink,
            &mut free,
            &mut nova,
        );
        let completion_cues = wd.drain_curse_cues().count();
        assert_eq!(completion_cues, 1, "complete `fuck` must cue once");
        assert_eq!(state["episode"], completion_cues as i64);

        // Negative control: `Buggy=1` (an episode already at `fuc`) diverges
        // from the real cue counts (none at `fuc`, one at `k`).
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut early = buggy.init_state();
        for action in ["TypeF", "TypeU", "TypeC"] {
            assert!(buggy.fire(action, &mut early), "{action}");
        }
        assert_ne!(early["episode"], prefix_cues as i64);
        assert!(buggy.fire("TypeK", &mut early));
        assert_ne!(early["episode"], completion_cues as i64);
        assert!(!model.check_invariant("CompletionCreatesExactlyOneEpisode", &early));
    }

    /// Passive output leaves the cursor after the word; after identity expiry
    /// the done mark still keeps it inert (a caret-only policy would re-arm).
    #[test]
    fn passive_output_at_terminal_cursor_stays_spent() {
        let lex = Lexicon::with_languages(&["all"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let t0 = Instant::now();
        let mut output = Terminal::new(4, 48);
        output.process(b"fuck");
        assert_eq!(output.cursor().col, 4, "word must end at terminal cursor");

        let mut wd = WordDecorations::default();
        wd.rescan(&output, 4, 48, &lex, &c, 1, t0);
        let ident = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("initial profanity")
            .ident;
        wd.persist
            .get_mut(&ident)
            .expect("initial episode")
            .nova_done = true;

        let blank = Terminal::new(4, 48);
        wd.rescan(&blank, 4, 48, &lex, &c, 2, t0 + Duration::from_secs(16));
        assert!(wd.persist.is_empty(), "fixture must cross identity expiry");
        wd.rescan(&output, 4, 48, &lex, &c, 3, t0 + Duration::from_secs(17));

        let restored = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("restored profanity");
        assert!(restored.inert, "passive cursor-end output rearmed");
        assert!(wd.persist[&restored.ident].born_done);
        assert!(wd.persist[&restored.ident].nova_done);
    }

    /// After a spent `fuck` and `fix`, `fuc` stays ordinary and a live `fuck`
    /// re-arms once; a second retype after `future` re-arms again.
    #[test]
    fn repeated_complete_fuck_rearms_while_fuc_stays_ordinary() {
        let lex = Lexicon::with_languages(&["all"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();

        let mut initial = Terminal::new(4, 48);
        initial.process(b"fuck");
        wd.rescan(&initial, 4, 48, &lex, &c, 1, t0);
        let initial_ident = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("initial profanity")
            .ident;
        wd.persist
            .get_mut(&initial_ident)
            .expect("initial episode")
            .nova_done = true;
        let births_before = wd.birth_seq;

        for (i, text) in ["f", "fi", "fix"].into_iter().enumerate() {
            let mut typing = Terminal::new(4, 48);
            typing.process(text.as_bytes());
            wd.rescan(
                &typing,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
            assert!(wd.occ.iter().all(|occ| occ.class != Class::Profanity));
        }
        assert!(wd.persist[&initial_ident].continuity_tainted);

        let first_retype_at = t0 + Duration::from_millis(64);
        let mut live = Terminal::new(4, 48);
        live.process(b"fuc");
        wd.rescan(&live, 4, 48, &lex, &c, 5, first_retype_at);
        assert!(wd.occ.iter().all(|occ| occ.class != Class::Profanity));
        assert_eq!(
            wd.birth_seq, births_before,
            "`fuc` must not create an episode"
        );

        live.process(b"k");
        // The `k` was typed: the profanity re-arm needs the keystroke too.
        wd.note_typed_edit(first_retype_at + Duration::from_millis(8), None);
        wd.rescan(
            &live,
            4,
            48,
            &lex,
            &c,
            6,
            first_retype_at + Duration::from_millis(8),
        );
        let completed = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("completed fuck")
            .clone();
        assert_eq!(
            completed.appeared,
            first_retype_at + Duration::from_millis(8)
        );
        assert_eq!(
            wd.birth_seq,
            births_before + 1,
            "completion is one fresh birth"
        );
        wd.persist
            .get_mut(&completed.ident)
            .expect("completed episode")
            .nova_done = true;

        for (i, text) in ["f", "fu", "fut", "futu", "futur", "future"]
            .into_iter()
            .enumerate()
        {
            let mut typing = Terminal::new(4, 48);
            typing.process(text.as_bytes());
            wd.rescan(
                &typing,
                4,
                48,
                &lex,
                &c,
                i as u64 + 7,
                t0 + Duration::from_millis(80 + 16 * i as u64),
            );
            assert!(
                wd.occ.iter().all(|occ| occ.class != Class::Profanity),
                "provisional {text:?} classified as profanity"
            );
        }

        let second_retype_at = t0 + Duration::from_millis(192);
        let mut repeated = Terminal::new(4, 48);
        repeated.process(b"fuck");
        // The first retype spent the witness; this one needs its own key.
        wd.note_typed_edit(second_retype_at, None);
        wd.rescan(&repeated, 4, 48, &lex, &c, 13, second_retype_at);
        let second = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("second live fuck");
        assert_eq!(second.appeared, second_retype_at);
        assert_eq!(
            wd.birth_seq,
            births_before + 2,
            "each live retype is one birth"
        );
        assert!(!second.inert);
        assert!(!wd.persist[&second.ident].nova_done);
    }

    /// Composer repaints while `fix` is typed, then a restored `fuck` line: the
    /// spent profanity does not re-arm.
    #[test]
    fn fix_churn_cannot_rearm_spent_profanity_on_restore() {
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha fuck tail");
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Profanity)
            .expect("positive-control profanity")
            .clone();
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

        for (i, text) in [b"f".as_slice(), b"fi".as_slice(), b"fix".as_slice()]
            .into_iter()
            .enumerate()
        {
            let mut composer = Terminal::new(4, 48);
            composer.process(text);
            wd.rescan(
                &composer,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
            assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
        }

        let mut restored_frame = Terminal::new(4, 48);
        restored_frame.process(b"STATUS fix alpha beta fuck gamma delta");
        let births_before = wd.birth_seq;
        wd.rescan(
            &restored_frame,
            4,
            48,
            &lex,
            &c,
            5,
            t0 + Duration::from_millis(64),
        );
        let restored = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Profanity)
            .expect("restored profanity line");
        assert_eq!(wd.birth_seq, births_before, "no synthetic profanity birth");
        assert_eq!(restored.appeared, t0, "the spent episode transfers");
        assert!(wd.persist[&restored.ident].nova_done, "no nova re-arm");
    }

    #[test]
    fn profanity_rainbow_opens_with_bounded_sparkles() {
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        c.supernova_chance = 0;
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 4,
            cols: 48,
        };
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 48);
        term.process(b"fuck");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 48, &lex, &c, 1, t0);
        let (mut deco, mut ink, mut free, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());

        wd.tick(
            t0 + Duration::from_millis(RAINBOW_SPARKLE_MS / 2),
            &c,
            geom,
            None,
            None,
            true,
            &mut deco,
            &mut ink,
            &mut free,
            &mut nova,
        );
        assert_eq!(
            deco.len(),
            3,
            "the rainbow opens with a small constellation"
        );
        assert!(deco.iter().all(|d| matches!(d.blend, DecoBlend::Add)));
        assert!(
            deco.iter()
                .all(|d| matches!(d.glyph, DecoGlyph::Star4 | DecoGlyph::Star5))
        );
        assert!(!ink.is_empty(), "sparkles accompany the rainbow ink");

        wd.tick(
            t0 + Duration::from_millis(RAINBOW_SPARKLE_MS + 1),
            &c,
            geom,
            None,
            None,
            true,
            &mut deco,
            &mut ink,
            &mut free,
            &mut nova,
        );
        assert!(
            deco.is_empty(),
            "sparkles self-terminate inside the ink drift"
        );
        assert!(
            !ink.is_empty(),
            "the rainbow may continue after the stars settle"
        );
    }

    /// Builtin `en` plus an emphasis override for `ultrathink`: what a user's
    /// `extra_words` compiles to (the builtin ships no emphasis forms).
    fn lex_with_emphasis() -> Lexicon {
        Lexicon::with_languages_and_override(
            &["en"],
            Some(
                "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n",
            ),
        )
        .expect("emphasis override parses")
    }

    /// `rescan_from_cells` over a `cell_frame_into` snapshot matches
    /// `rescan(&term)` byte for byte, wide (CJK) and DECDWL rows included.
    #[test]
    fn rescan_from_cells_matches_term_rescan() {
        let (rows, cols) = (5usize, 48usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"I love cats and say fuck sometimes\r\n");
        // Wide CJK leads shift the char->column map ahead of the matches.
        term.process("\u{5BBD}\u{5BBD} kitty ultrathink\r\n".as_bytes());
        // DECDWL: the double-width row must carry `dec_line` on both paths.
        term.process(b"\x1b#6cat nap\r\n");
        term.process(b"plain row, nothing to match");
        let lex = Lexicon::with_languages(&["en"]);
        let c = cfg();
        let epoch = term.damage_epoch();
        let now = Instant::now();

        let mut wd_term = WordDecorations::default();
        wd_term.rescan(&term, rows, cols, &lex, &c, epoch, now);

        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let mut wd_cells = WordDecorations::default();
        wd_cells.rescan_from_cells(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            epoch,
            now,
        );
        assert!(
            !wd_cells.needs_rescan(epoch),
            "snapshot scan must record the epoch"
        );

        let key = |o: &Occurrence| {
            (
                o.row,
                o.start_col,
                o.end_col,
                o.class,
                o.seed,
                o.ident,
                o.appeared,
                o.genome,
                o.ink_base,
                o.ink_cells,
                o.ink_bg,
                o.dec_line,
            )
        };
        assert!(
            !wd_term.occ.is_empty(),
            "the fixture must actually match words"
        );
        assert!(
            wd_term.occ.iter().any(|o| o.dec_line),
            "the DECDWL row's match must flag dec_line"
        );
        assert!(
            wd_term.occ.iter().any(|o| o.ink_cells > 0),
            "the fixture must capture ink"
        );
        assert_eq!(
            wd_term.occ.iter().map(key).collect::<Vec<_>>(),
            wd_cells.occ.iter().map(key).collect::<Vec<_>>(),
            "snapshot scan must match the term-walking scan"
        );
        assert_eq!(
            wd_term.occ.iter().map(|o| o.cat_colors).collect::<Vec<_>>(),
            wd_cells
                .occ
                .iter()
                .map(|o| o.cat_colors)
                .collect::<Vec<_>>(),
            "snapshot scan must resolve the same local cat palettes"
        );
        assert_eq!(
            wd_term.ink_base_fg, wd_cells.ink_base_fg,
            "captured base fg must match"
        );
        assert_eq!(
            wd_term.ink_cols, wd_cells.ink_cols,
            "captured ink columns must match"
        );
    }

    /// Every decoration field the renderer reads plus the ink buffers they
    /// index: the [`ScanMemo`] battery's equivalence surface.
    fn deco_state(wd: &WordDecorations) -> (Vec<String>, Vec<[u8; 3]>, Vec<u16>, Vec<u64>) {
        let occ = wd
            .occ
            .iter()
            .map(|o| {
                // Two groups: `Debug` is only implemented for tuples up to 12.
                format!(
                    "{:?}{:?}",
                    (
                        o.row,
                        o.start_col,
                        o.end_col,
                        o.class,
                        o.form_id,
                        o.seed,
                        o.ident,
                        o.appeared,
                        o.genome,
                    ),
                    (
                        o.ink_base,
                        o.ink_cells,
                        o.ink_bg,
                        o.cat_colors,
                        o.cat_text_clear,
                        o.dec_line,
                        o.inert,
                        o.custom,
                    )
                )
            })
            .collect();
        // Episode identities too: a stale match list shows up as a different
        // persist population even when visible occurrences coincide.
        let mut idents: Vec<u64> = wd.persist.keys().copied().collect();
        idents.sort_unstable();
        (occ, wd.ink_base_fg.clone(), wd.ink_cols.clone(), idents)
    }

    /// A memoized engine and an always-re-lex engine agree on every rescan
    /// through partial words at the caret, soft wraps, wide CJK leads, DECDWL
    /// rows, scrolling, repeated lines and a clear.
    #[test]
    fn scan_memo_frames_are_identical_to_an_always_rescanning_engine() {
        let (rows, cols) = (14usize, 44usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        let lex = Lexicon::with_languages(&["en"]);
        let c = cfg();
        let geom = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut memoized = WordDecorations::default();
        let mut control = WordDecorations::default();
        control.scan_memo.bypass = true;
        let t0 = Instant::now();

        // Each step is one PTY write; the rescan runs after every one, exactly
        // as the per-frame render path does under streaming output.
        let steps: &[&[u8]] = &[
            b"I love cats and say fuck sometimes\r\n",
            b"a kitty naps here\r\n",
            b"plain build output, nothing to match\r\n",
            "\u{5BBD}\u{5BBD} kitty ultrathink\r\n".as_bytes(),
            b"\x1b#6cat nap\r\n",
            // Incremental typing: `fu`/`fuc` sit under the caret (the
            // provisional-collision gate) before `fuck` settles.
            b"$ fu",
            b"c",
            b"k",
            b" ",
            b"and a cat",
            b"\r\n",
            // Repeated identical lines: the memo answers every one of them.
            b"the cat sat on the mat\r\n",
            b"the cat sat on the mat\r\n",
            b"the cat sat on the mat\r\n",
            // Overflow the viewport so the whole screen scrolls: every row's
            // text survives at a new row index.
            b"scroll me one\r\nscroll me two\r\nfuck this build\r\nkitty again\r\n",
            b"more cats\r\nmore kittens\r\nmore fuck\r\n",
            // Clear + redraw: the grace/alignment machinery, on a memo that
            // still holds every one of the cleared lines.
            b"\x1b[2J\x1b[H",
            b"kitty again\r\n",
        ];

        let mut snap = aterm_core::render::RenderInput::default();
        for (i, step) in steps.iter().enumerate() {
            term.process(step);
            term.cell_frame_into(&mut snap, rows, cols);
            let now = t0 + Duration::from_millis(16 * (i as u64 + 1));
            let cursor = (term.grid().display_offset() == 0).then(|| {
                let cur = term.cursor();
                (cur.row, cur.col)
            });
            let epoch = term.damage_epoch();
            for wd in [&mut memoized, &mut control] {
                wd.rescan_from_cells_with_geom_at_cursor(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    &lex,
                    &c,
                    epoch,
                    now,
                    geom,
                    0x0011_1111,
                    cursor,
                );
            }
            assert_eq!(
                deco_state(&memoized),
                deco_state(&control),
                "frame {i}: the memoized rescan diverged from the full rescan"
            );
        }
        assert!(
            !memoized.occ.is_empty(),
            "the fixture must end with live decorations"
        );
        // Non-vacuity: the battery must actually have been served from the
        // memo, or the equality above proves nothing about it.
        assert!(
            memoized.scan_memo.hits > u64::try_from(steps.len()).expect("small") * 4,
            "the memo must serve the bulk of the rows (hits: {}, misses: {})",
            memoized.scan_memo.hits,
            memoized.scan_memo.misses
        );
        assert_eq!(
            control.scan_memo.hits, 0,
            "the control engine must never be served from the memo"
        );
    }

    /// A deny-list edit or a language switch on an idle grid retires the memo
    /// (`ScanMemo::fit` over [`scan_inputs_fp`], which covers both the scan
    /// gates and the lexicon), or a newly ignored word (or a switched-off
    /// language's form) would keep decorating. A two-way gate, not a latch.
    #[test]
    fn scan_memo_retires_when_the_gates_or_the_lexicon_change() {
        let (rows, cols) = (3usize, 32usize);
        let en = Lexicon::with_languages(&["en"]);
        // `futu` is a `ro`-gated ambiguous profanity form: present in the
        // en+ro lexicon, absent from the en-only one, with the same gates.
        let en_ro = Lexicon::with_languages(&["en", "ro"]);
        let c = cfg();
        let mut ignoring = cfg();
        ignoring.ignore.insert("kitty".to_string());
        for (label, text, class, (lex_on, c_on), (lex_off, c_off)) in [
            (
                "deny list",
                "the kitty is here\r\n",
                Class::Feline,
                (&en, &c),
                (&en, &ignoring),
            ),
            (
                "lexicon",
                "totally futu here\r\n",
                Class::Profanity,
                (&en_ro, &c),
                (&en, &c),
            ),
        ] {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(text.as_bytes());
            let mut snap = aterm_core::render::RenderInput::default();
            term.cell_frame_into(&mut snap, rows, cols);
            let now = Instant::now();
            let mut wd = WordDecorations::default();
            // Same cells every time; only the scan inputs change.
            let scans = [
                (lex_on, c_on, true),
                (lex_off, c_off, false),
                (lex_on, c_on, true),
            ];
            for (epoch, (lex, c, decorated)) in scans.into_iter().enumerate() {
                let epoch = epoch as u64 + 1;
                wd.rescan_from_cells(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    lex,
                    c,
                    epoch,
                    now,
                );
                assert_eq!(
                    wd.occ.iter().any(|o| o.class == class),
                    decorated,
                    "{label}: scan {epoch} must follow its own inputs, not a stale memo"
                );
            }
        }
    }

    /// The probe class of hash `h` in a 256-bucket hashbrown table: starting
    /// bucket and 7-bit control tag. Keys sharing one form a chain.
    fn probe_class(h: u64) -> (u64, u64) {
        (h & 0xff, h >> 57)
    }

    /// The most `keys` that any one probe class holds under `hash`.
    fn largest_probe_class<K>(keys: &[K], hash: impl Fn(&K) -> u64) -> usize {
        let mut classes: FxHashMap<(u64, u64), usize> = FxHashMap::default();
        keys.iter()
            .map(|k| {
                let n = classes.entry(probe_class(hash(k))).or_default();
                *n += 1;
                *n
            })
            .max()
            .unwrap_or(0)
    }

    /// 128 of `candidate`'s keys sharing probe class `(0, 0)` under unkeyed Fx
    /// (asserted, so the set is one full chain). Under a keyed hasher, 9+ of
    /// 128 in one of 2^15 classes has probability below 2^-70, so a bound of 8
    /// is no flake.
    fn fx_flood<K: std::hash::Hash>(candidate: impl Fn(u32) -> K) -> Vec<K> {
        use std::hash::BuildHasher;
        let fx = |k: &K| aterm_hash::FxBuildHasher.hash_one(k);
        let mut flood = Vec::with_capacity(128);
        let mut i = 0u32;
        while flood.len() < 128 {
            let k = candidate(i);
            if probe_class(fx(&k)) == (0, 0) {
                flood.push(k);
            }
            i += 1;
        }
        assert_eq!(largest_probe_class(&flood, fx), flood.len());
        flood
    }

    /// Rows precomputed to share one Fx bucket and tag ([`fx_flood`]) spread
    /// under the memo's keyed hashers.
    #[test]
    fn scan_memo_spreads_rows_precomputed_to_collide() {
        use std::hash::BuildHasher;
        let flood = fx_flood(|mut i| {
            let mut s = String::from("row ");
            for _ in 0..6 {
                s.extend(char::from_u32(u32::from(b'a') + i % 26));
                i /= 26;
            }
            s
        });

        let mut memo = ScanMemo::default();
        memo.fit(64, 0);
        for r in &flood {
            assert!(!memo.touch(r));
            memo.remember(r, &[]);
        }
        for r in &flood {
            assert!(memo.touch(r), "a remembered row is resident");
        }
        for (generation, map) in [("hot", &memo.hot), ("cold", &memo.cold)] {
            let n = largest_probe_class(&flood, |r| map.hasher().hash_one(r.as_str()));
            assert!(
                n <= 8,
                "{generation}: {n} of {} precomputed rows share one bucket and tag, \
                 so a probe among them compares that many rows",
                flood.len()
            );
        }
    }

    /// Done marks (`ident ^ ctx_fp`, a SimHash of program output) precomputed
    /// to share one Fx bucket and tag ([`fx_flood`]) spread under the LRU
    /// index's keyed hasher.
    #[test]
    fn done_marks_spread_keys_precomputed_to_collide() {
        use std::hash::BuildHasher;
        let ident = mix(seed_of(
            10,
            Class::Feline,
            aterm_lexicon::form_hash("kitty"),
        ));
        let flood = fx_flood(|i| ident ^ mix(u64::from(i)));

        let mut marks = DoneMarkLru::default();
        for &key in &flood {
            assert_eq!(marks.insert(key).evicted, None);
        }
        for key in &flood {
            assert!(marks.contains_key(key), "a written mark is resident");
        }
        let n = largest_probe_class(&flood, |k| marks.index.hasher().hash_one(k));
        assert!(
            n <= 8,
            "{n} of {} precomputed done marks share one bucket and tag, \
             so a birth's probe among them compares that many marks",
            flood.len()
        );
    }

    #[test]
    fn geometry_scan_samples_and_freezes_the_prospective_cat_footprint() {
        let (rows, cols) = (4usize, 20usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\r\n\r\nkitty");
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let dark = [4, 4, 4];
        let light = [248, 248, 248];
        for (r, line) in snap.cells.iter_mut().enumerate() {
            for cell in line {
                cell.bg = if r == 0 { light } else { dark };
            }
        }
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: rows as u16,
            cols: cols as u16,
        };
        let lex = lex();
        let c = cfg();
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            1,
            now,
            geom,
            rgb3_to_u32(light),
            None,
        );
        let mixed = feline(&wd).cat_colors;
        let direct = cat_color_context_footprint(
            &snap.cells,
            geom,
            feline(&wd).row,
            feline(&wd).start_col,
            feline(&wd).end_col,
            feline(&wd).genome,
            c.feline_magic,
            rgb3_to_u32(light),
        );
        assert_eq!(mixed, direct, "rescan stores the footprint sampler's key");
        assert!(
            !mixed.dark(),
            "the dark+light footprint uses a dedicated non-dark mixed class"
        );
        assert_eq!(mixed.background, 4);

        for line in &mut snap.cells {
            for cell in line {
                cell.bg = dark;
            }
        }
        // The background under the footprint changes; the live episode keeps
        // the palette it froze at birth.
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            2,
            now + Duration::from_millis(100),
            geom,
            rgb3_to_u32(dark),
            None,
        );
        assert_eq!(
            feline(&wd).cat_colors,
            mixed,
            "a live episode freezes its footprint-derived palette"
        );
        let mut fresh = WordDecorations::default();
        fresh.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            1,
            now,
            geom,
            rgb3_to_u32(dark),
            None,
        );
        assert!(
            feline(&fresh).cat_colors.dark(),
            "a fresh episode sees the now-dark full footprint"
        );
    }

    /// Rescan + tick a real grid, returning the ink for one frame.
    fn ink_at(
        wd: &mut WordDecorations,
        term: &mut Terminal,
        rows: usize,
        cols: usize,
        c: &DecoConfig,
        scanned: Instant,
        at: Instant,
    ) -> (Vec<InkCell>, u64) {
        let lex = lex_with_emphasis();
        let epoch = term.damage_epoch();
        // A hard reset, so repeated runs are not born-done (a transient reset
        // keeps done marks).
        wd.hard_reset();
        wd.rescan(term, rows, cols, &lex, c, epoch, scanned);
        let mut out = Vec::new();
        let mut ink = Vec::new();
        let fp = tick_ink(wd, at, c, &mut out, &mut ink);
        (ink, fp)
    }

    /// `tick` with a throwaway sprite scratch + zero geometry (no cat graphic:
    /// the §5.7 floor trips), for the ink-focused tests.
    fn tick_ink(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        out: &mut Vec<WordDecoration>,
        ink: &mut Vec<InkCell>,
    ) -> u64 {
        let mut fr = Vec::new();
        let mut nova = Vec::new();
        wd.tick(
            now,
            cfg,
            EffectGeom::default(),
            None,
            None,
            true,
            out,
            ink,
            &mut fr,
            &mut nova,
        )
    }

    /// After `sweep_ms + 250 ms` the ink bytes and fingerprint are constant,
    /// `is_active` disarms, and the bytes equal the reduced-motion gradient.
    #[test]
    fn ink_settles_to_constant_bytes_and_disarms() {
        let mut term = Terminal::new(2, 48);
        term.process(b"ultrathink");
        let mut wd = WordDecorations::default();
        let c = cfg();
        let now = Instant::now();
        let (live, fp_live) = ink_at(&mut wd, &mut term, 2, 48, &c, now, now);
        assert_eq!(live.len(), 10, "one InkCell per glyph of `ultrathink`");
        assert!(wd.is_active(now), "a live sweep must arm the scheduler");
        // Two consecutive frames mid-sweep differ (the frame-XOR anti-skip rule).
        let mut out = Vec::new();
        let mut ink2 = Vec::new();
        let fp_live2 = tick_ink(
            &mut wd,
            now + Duration::from_millis(16),
            &c,
            &mut out,
            &mut ink2,
        );
        assert_ne!(fp_live, fp_live2, "animating ink fp changes each frame");
        // Past sweep_ms + 250 ms: constant bytes, stable fp, not active.
        let later = now + Duration::from_millis(u64::from(c.ink_sweep_ms) + 250 + 250);
        let fp_a = tick_ink(&mut wd, later, &c, &mut out, &mut ink2);
        let settled = ink2.clone();
        let fp_b = tick_ink(
            &mut wd,
            later + Duration::from_millis(16),
            &c,
            &mut out,
            &mut ink2,
        );
        assert_eq!(fp_a, fp_b, "settled ink fingerprint is stable");
        assert_eq!(settled, ink2, "settled ink is byte-constant across frames");
        assert!(
            !wd.is_active(later),
            "settled ink must disarm the scheduler"
        );
        // Endpoint identity: settled == reduced-motion static gradient.
        let mut rm = cfg();
        rm.reduced_motion = true;
        let (static_ink, _) = ink_at(&mut wd, &mut term, 2, 48, &rm, now, now);
        assert_eq!(
            settled, static_ink,
            "settled sweep == static gradient bytes"
        );
    }

    /// §4.4: `MAX_INK_CELLS` truncates whole words row-major (never partial)
    /// and re-emission is a stable fixed point. Under `reduced_motion` that
    /// static gradient holds from frame 0 and never arms the scheduler.
    #[test]
    fn ink_truncation_is_whole_word_and_stable() {
        // 4 × 10-col words per row = 40 lead cells/row: 12 rows + 3 words fill
        // 510 of 512, so word 52 gets no ink rather than 2 cells.
        let rows = 20usize;
        let cols = 44usize;
        let mut term = Terminal::new(rows as u16, cols as u16);
        let line = b"ultrathink ultrathink ultrathink ultrathink".to_vec();
        for r in 0..rows {
            if r > 0 {
                term.process(b"\r\n");
            }
            term.process(&line);
        }
        let mut wd = WordDecorations::default();
        let mut c = cfg();
        c.reduced_motion = true; // settled bytes, so re-emission compares exactly
        let now = Instant::now();
        let (ink, fp_a) = ink_at(&mut wd, &mut term, rows, cols, &c, now, now);
        assert!(!wd.is_active(now), "reduced-motion ink never animates");
        assert_eq!(ink.len(), 510, "51 whole words of 10 cells, never partial");
        assert!(
            ink.iter().filter(|i| i.row == 12).count() == 30 && ink.iter().all(|i| i.row <= 12),
            "row-major prefix: 3 words on row 12, nothing below"
        );
        // Sorted-unique (row, col): the renderer merge-walk invariant.
        assert!(
            ink.windows(2)
                .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col))
        );
        let mut out = Vec::new();
        let mut again = Vec::new();
        let fp_b = tick_ink(
            &mut wd,
            now + Duration::from_millis(16),
            &c,
            &mut out,
            &mut again,
        );
        assert_eq!(ink, again, "truncated emission is a stable fixed point");
        assert_eq!(fp_a, fp_b);
    }

    /// §4.3 legibility guard: pastel ink on a light background pulls toward the
    /// base fg in eighth-steps until the mid-gradient clears `MIN_INK_CONTRAST`.
    #[test]
    fn ink_legibility_guard_pulls_on_light_bg() {
        let mut term = Terminal::new(2, 48);
        // Dark text on a near-white bg: the theme's own fg is legible, the
        // full-strength pastel ink pair is not.
        term.process(b"\x1b[38;2;30;30;30m\x1b[48;2;250;250;250multrathink");
        let mut wd = WordDecorations::default();
        let mut c = cfg();
        c.reduced_motion = true; // static gradient isolates the guard from the sweep
        c.ink_strength = 1.0; // full replacement: the raw pastel pair CANNOT pass on near-white
        let now = Instant::now();
        let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &c, now, now);
        assert_eq!(ink.len(), 10);
        // The deployed anchors are the genome-nudged pair (§4.2), read off the
        // occurrence's frozen genome.
        let (c0, c1) = nudged_ink_pair(Class::Emphasis, wd.occ[0].genome.gkey).unwrap();
        let (fg, bg) = (rgb3_to_u32([30, 30, 30]), rgb3_to_u32([250, 250, 250]));
        // Replicate the guard: pull strength in eighth-steps until the
        // mid-gradient ink clears the bound.
        let mid = mix_rgb(c0, c1, 0.5);
        let mut s = c.ink_strength;
        while s > 0.0 && contrast_ratio(mix_rgb(fg, mid, s), bg) < MIN_INK_CONTRAST {
            s = (s - c.ink_strength / 8.0).max(0.0);
        }
        assert!(
            s < c.ink_strength,
            "the pastel pair on near-white must actually engage the pull"
        );
        assert!(contrast_ratio(mix_rgb(fg, mid, s), bg) >= MIN_INK_CONTRAST);
        // The emitted bytes are exactly the guarded mix, per cell (u = i/9).
        for (i, cell) in ink.iter().enumerate() {
            let expect = mix_rgb(fg, mix_rgb(c0, c1, i as f32 / 9.0), s);
            assert_eq!(cell.color, u32_to_rgb3(expect), "cell {i} guarded color");
        }
    }

    /// `emphasis = false` drops the class at rescan; `ink_enabled = false`
    /// emits no ink for any class.
    #[test]
    fn emphasis_and_ink_gates() {
        let mut term = Terminal::new(2, 48);
        term.process(b"ultrathink and a kitten say fuck");
        let mut wd = WordDecorations::default();
        let now = Instant::now();
        // Feline ink is a glow pulse from 0, so sample at its first peak
        // (350 ms), where all three ink-bearing classes are live.
        let lit = now + Duration::from_millis(350);
        // All gates on: ink covers emphasis + feline + profanity glyph spans.
        let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &cfg(), now, lit);
        assert_eq!(
            ink.len(),
            10 + 6 + 4,
            "ultrathink + kitten + fuck lead cells, got {ink:?}"
        );
        // Emphasis off: only the feline + profanity spans keep ink.
        let mut c = cfg();
        c.emphasis = false;
        let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &c, now, lit);
        assert_eq!(ink.len(), 6 + 4, "emphasis occurrences dropped at rescan");
        // Ink off: nothing emits, whatever the classes matched.
        let mut c = cfg();
        c.ink_enabled = false;
        let (ink, fp) = ink_at(&mut wd, &mut term, 2, 48, &c, now, lit);
        assert!(ink.is_empty(), "ink_enabled = false emits zero InkCells");
        // The paw + sparkles still fold a fingerprint (deco path untouched).
        assert_ne!(fp, 0);
    }

    /// After `reset()`, the next tick with the same scratch returns empty ink
    /// and fp 0 (the suppressed-alt-screen / master-off arms).
    #[test]
    fn reset_clears_stale_ink_on_next_tick() {
        let mut term = Terminal::new(2, 48);
        term.process(b"ultrathink");
        let mut wd = WordDecorations::default();
        let c = cfg();
        let now = Instant::now();
        let lex = lex_with_emphasis();
        let epoch = term.damage_epoch();
        wd.rescan(&term, 2, 48, &lex, &c, epoch, now);
        let mut out = Vec::new();
        let mut ink = Vec::new();
        tick_ink(&mut wd, now, &c, &mut out, &mut ink);
        assert!(!ink.is_empty());
        wd.reset(); // the suppressed-alt-screen (vim) entry / master-off path
        let fp = tick_ink(&mut wd, now, &c, &mut out, &mut ink);
        assert_eq!(fp, 0);
        assert!(ink.is_empty(), "stale ink must clear on the next tick");
        assert!(!wd.is_active(now));
    }

    // ───────────────── §3.6 genome / persistence battery ─────────────────

    fn lex() -> Lexicon {
        Lexicon::with_languages(&["en"])
    }

    /// The one feline occurrence of the current scan.
    fn feline(wd: &WordDecorations) -> &Occurrence {
        wd.occ
            .iter()
            .find(|o| o.class == Class::Feline)
            .expect("a feline occurrence")
    }

    /// §13 `genome_frozen_across_neighbor_churn`: neighbor and SGR churn keep the
    /// frozen genome and palette; after grace expiry the word resolves anew.
    #[test]
    fn genome_frozen_across_neighbor_churn() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 64);
        term.process(b"\x1b[31ma happy kitty sits here");
        wd.rescan(&term, 2, 64, &lex, &c, 1, t0);
        let g1 = feline(&wd).genome;
        let colors1 = feline(&wd).cat_colors;

        // Same word/column, churned neighbor and red→blue surrounding SGR.
        let mut churned = Terminal::new(2, 64);
        churned.process(b"\x1b[34ma grump kitty sits here");
        wd.rescan(
            &churned,
            2,
            64,
            &lex,
            &c,
            2,
            t0 + Duration::from_millis(500),
        );
        assert_eq!(feline(&wd).genome, g1, "persist hit must freeze the genome");
        assert_eq!(
            feline(&wd).cat_colors,
            colors1,
            "persist hit must freeze the atlas palette key"
        );
        assert_eq!(feline(&wd).appeared, t0, "hit keeps the episode's appeared");

        // True identity death: unseen past the grace TTL ⇒ swept at rescan end.
        let blank = Terminal::new(2, 64);
        wd.rescan(&blank, 2, 64, &lex, &c, 3, t0 + Duration::from_secs(12));
        assert!(wd.persist.is_empty(), "grace expiry sweeps the episode");

        // Re-appearance rolls a fresh genome from the churned context.
        wd.rescan(&churned, 2, 64, &lex, &c, 4, t0 + Duration::from_secs(13));
        assert_ne!(
            feline(&wd).genome.gkey,
            g1.gkey,
            "rebirth re-rolls from the new neighbors"
        );
        assert_ne!(
            feline(&wd).cat_colors,
            colors1,
            "rebirth resolves the new surrounding colors"
        );
        assert_eq!(feline(&wd).appeared, t0 + Duration::from_secs(13));
    }

    /// §13 allocation regression: every resident scratch buffer's capacity is
    /// stable across 1000 rescans + ticks (zero steady-state allocation).
    #[test]
    fn allocation_regression_resident_capacities_stable() {
        let lex = lex_with_emphasis();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term_a = Terminal::new(4, 64);
        term_a.process(b"a happy kitty says fuck\r\nultrathink beats the orca");
        let mut term_b = Terminal::new(4, 64);
        // Same surfaces and contexts, shifted horizontally by one cell: this
        // forces the rekey alignment path on every alternating rescan.
        term_b.process(b" a happy kitty says fuck\r\n ultrathink beats the orca");

        let caps = |wd: &WordDecorations| {
            [
                wd.occ.capacity(),
                wd.scan_cells.capacity(),
                wd.scan_chars.capacity(),
                wd.scan_matches.capacity(),
                wd.scan_text.capacity(),
                wd.scan_colmap.capacity(),
                wd.scan_row_occupied.capacity(),
                wd.ink_base_fg.capacity(),
                wd.ink_cols.capacity(),
                wd.persist.capacity(),
                wd.seed_ordinals.capacity(),
                wd.ctx_tok.capacity(),
                wd.ctx_folded.capacity(),
                wd.pending.capacity(),
                wd.align_old.capacity(),
                wd.align_decisions.capacity(),
                // The memo too: with every row memoized a rescan neither
                // inserts nor rotates, so growth means it misses every frame.
                wd.scan_memo.hot.capacity(),
                wd.scan_memo.cold.capacity(),
            ]
        };
        let mut out = Vec::new();
        let mut ink = Vec::new();
        // Warmup: both grids' identities enter the persist map and every
        // resident buffer reaches its high-water capacity.
        wd.rescan(&term_a, 4, 64, &lex, &c, 1, t0);
        tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
        wd.rescan(&term_b, 4, 64, &lex, &c, 2, t0);
        tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
        let warm = caps(&wd);
        // The DP decision plane is lent via `mem::take`; a missed handback
        // would reallocate it every frame yet pass the capacity check (0 == 0).
        assert!(
            wd.align_decisions.capacity() >= ALIGN_DECISION_CELLS,
            "align_pending must hand its decision plane back"
        );
        for i in 0..1000u64 {
            let term = if i % 2 == 0 { &term_a } else { &term_b };
            wd.rescan(term, 4, 64, &lex, &c, 3 + i, t0);
            tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
        }
        assert_eq!(
            caps(&wd),
            warm,
            "resident capacities must not grow after warmup"
        );
    }

    /// §13 allocation, miss side: an all-new full-screen repaint misses every
    /// probe, yet after warm-up refills retired buffers (`fresh_buffers` stays).
    #[test]
    fn allocation_regression_all_miss_repaint_reuses_retired_buffers() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let (rows, cols) = (8usize, 48usize);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut snap = aterm_core::render::RenderInput::default();
        let mut line = String::new();
        // One full-screen repaint: every row carries the frame number, so no
        // row's text can ever be answered from the memo.
        let mut repaint = |wd: &mut WordDecorations, term: &mut Terminal, e: u64| {
            term.process(b"\x1b[H");
            for r in 0..rows {
                line.clear();
                // Fixed-width frame tag: a widening counter would shift match
                // columns, which seed identities (unrelated persist churn).
                for digit in [1000u64, 100, 10, 1] {
                    let d = u8::try_from(e / digit % 10).expect("one digit");
                    line.push(char::from(b'0' + d));
                }
                line.push(' ');
                line.push_str(&r.to_string());
                line.push_str(" a happy kitty says fuck");
                term.process(line.as_bytes());
                if r + 1 < rows {
                    term.process(b"\r\n");
                }
            }
            term.cell_frame_into(&mut snap, rows, cols);
            wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, e, t0);
        };
        // Warm-up: several full generations (a rotation every `gen_cap / rows`
        // frames) plus the episode machinery's high-water capacities.
        for e in 1..=60u64 {
            repaint(&mut wd, &mut term, e);
        }
        let caps = |wd: &WordDecorations| {
            [
                wd.scan_memo.hot.capacity(),
                wd.scan_memo.cold.capacity(),
                wd.scan_memo.spare.capacity(),
                wd.scan_text.capacity(),
                wd.scan_chars.capacity(),
                wd.scan_matches.capacity(),
                wd.scan_colmap.capacity(),
                wd.occ.capacity(),
                wd.persist.capacity(),
            ]
        };
        let warm = caps(&wd);
        let fresh = wd.scan_memo.fresh_buffers;
        for e in 61..=260u64 {
            repaint(&mut wd, &mut term, e);
        }
        assert_eq!(
            wd.scan_memo.hits, 0,
            "the fixture must miss on every row, or it is not measuring the miss path"
        );
        assert_eq!(
            wd.scan_memo.misses,
            260 * rows as u64,
            "every row of every frame must have reached the scanner"
        );
        assert_eq!(
            wd.scan_memo.fresh_buffers, fresh,
            "a warmed all-miss repaint must refill retired buffers, not allocate new ones"
        );
        assert_eq!(
            caps(&wd),
            warm,
            "resident capacities must not grow on the all-miss path either"
        );

        // Non-vacuity: an engine that allocates per miss buys a key + match-list
        // pair for every row of every frame.
        let mut control = WordDecorations::default();
        control.scan_memo.no_recycle = true;
        let mut control_term = Terminal::new(rows as u16, cols as u16);
        for e in 1..=260u64 {
            repaint(&mut control, &mut control_term, e);
        }
        assert_eq!(
            control.scan_memo.fresh_buffers,
            260 * rows as u64,
            "the control must allocate one buffer pair per missed row"
        );
        assert!(
            fresh < 260 * rows as u64 / 10,
            "the pooled engine must allocate only while it warms up (fresh: {fresh})"
        );
    }

    // ────────────── SparkleIdentity Tier-1 conformance (design §9) ──────────────

    /// Project the real persist map onto the model's observables and assert
    /// they match after each transition. `rolls` counts stored fresh genomes;
    /// `nova` is compared only while the episode exists.
    fn assert_projection(
        wd: &WordDecorations,
        ident: u64,
        rolls: i64,
        st: &std::collections::BTreeMap<&'static str, i64>,
    ) {
        let live = wd.occ.iter().any(|o| mix(o.seed) == ident);
        let state = if live {
            1
        } else if wd.persist.contains_key(&ident) {
            2
        } else {
            0
        };
        assert_eq!(state, st[&"state"], "projected state must match the model");
        assert_eq!(
            rolls, st[&"rolls"],
            "accepted fresh-episode genome generations must match the model"
        );
        if state != 0 {
            // Born-done pre-spends `nova_done` without a fire; the model counts
            // fires, so it projects as 0.
            let ep = &wd.persist[&ident];
            let nova = i64::from(ep.nova_done && !ep.born_done);
            assert_eq!(nova, st[&"nova"], "projected nova must match the model");
        }
    }

    /// Frames for the repeated-surface proof: two spent `kitty`s, `moved`
    /// (new neighbors, rows, columns; same count), `grown` (one more).
    fn repeated_kitty_reflow_frames() -> (Terminal, Terminal, Terminal) {
        let mut original = Terminal::new(6, 48);
        original.process(b"\x1b[1;1Hold alpha kitty tail\x1b[3;1Hold bravo kitty tail");

        let mut moved = Terminal::new(6, 48);
        // Both episodes move down a row; old row 2 is equidistant from new rows
        // 1 and 3 and must take row 3 after row 0 wins row 1.
        moved.process(b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps");

        let mut grown = Terminal::new(6, 48);
        grown.process(
            b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps\x1b[6;1Hnew omega kitty glows",
        );
        (original, moved, grown)
    }

    /// Tier-1: drive the real persist map through appear, ignite, rekeys,
    /// vanish, grace re-hit, expiry and re-appear alongside the
    /// `SparkleIdentity` model, checking `(state, rolls, nova)` at every step.
    #[test]
    fn sparkle_identity_conformance_real_persist_map() {
        let m = aterm_models_effects::sparkle_identity_model();
        let mut st = m.init_state();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 48);
        term.process(b"a fluffy kitty naps");
        let blank = Terminal::new(2, 48);
        let mut rolls = 0i64;

        // Appear: the first rescan is a persist miss — the one genome roll.
        assert!(wd.persist.is_empty());
        wd.rescan(&term, 2, 48, &lex, &c, 1, t0);
        rolls += 1;
        assert!(m.fire("Appear", &mut st));
        let mut ident = mix(feline(&wd).seed); // single occurrence, ordinal 0
        let g1 = feline(&wd).genome;
        assert_eq!(feline(&wd).appeared, t0);
        assert_projection(&wd, ident, rolls, &st);

        // Ignite (simulated, while live): the episode's one nova is spent.
        wd.persist.get_mut(&ident).expect("live episode").nova_done = true;
        assert!(m.fire("Ignite", &mut st));
        assert_projection(&wd, ident, rolls, &st);
        assert!(m.check_invariant("OneNovaPerEpisode", &st));

        // Rekey: an indented redraw changes the ident but transfers the episode
        // (no roll, no `appeared` reset, nova stays spent).
        let mut moved = Terminal::new(2, 48);
        moved.process(b" a fluffy kitty naps");
        let old_ident = ident;
        wd.rescan(&moved, 2, 48, &lex, &c, 2, at(1));
        assert!(m.fire("Rekey", &mut st));
        ident = mix(feline(&wd).seed);
        assert_ne!(ident, old_ident, "the horizontal move rekeys the map");
        assert!(
            !wd.persist.contains_key(&old_ident),
            "the old key was moved"
        );
        assert_projection(&wd, ident, rolls, &st);
        assert_eq!(feline(&wd).genome, g1, "Rekey preserves the frozen genome");
        assert_eq!(
            feline(&wd).appeared,
            t0,
            "Rekey preserves the episode clock"
        );
        assert!(
            wd.persist[&ident].nova_done,
            "Rekey preserves the spent guard"
        );
        assert!(m.check_invariant("PlayedOnce", &st));

        // ContextMove: a changed status token and the 48-column wrap move both
        // position and context; the probe proves the context really differs.
        let mut context_moved = Terminal::new(2, 48);
        context_moved
            .process(b"build output changed; wrapping follows here.    status a bright kitty naps");
        let frozen_ctx = wd.persist[&ident].ctx_fp;
        let mut probe = WordDecorations::default();
        probe.rescan(&context_moved, 2, 48, &lex, &c, 1, t0);
        let candidate_ctx = probe.persist[&mix(feline(&probe).seed)].ctx_fp;
        assert_ne!(
            candidate_ctx, frozen_ctx,
            "the wrapped status token must change the row-local fingerprint"
        );
        assert_eq!(
            feline(&probe).form_id,
            feline(&wd).form_id,
            "recognition evidence is the unchanged exact lexical surface"
        );

        let old_ident = ident;
        let births_before = wd.birth_seq;
        wd.rescan(&context_moved, 2, 48, &lex, &c, 3, at(2));
        assert!(m.fire("ContextMove", &mut st));
        ident = mix(feline(&wd).seed);
        assert_ne!(ident, old_ident, "the wrapped move changes the map key");
        assert_eq!(feline(&wd).row, 1, "the logical occurrence crossed a wrap");
        assert!(
            !wd.persist.contains_key(&old_ident),
            "recognition transfers rather than retaining a stale old episode"
        );
        assert_eq!(
            wd.birth_seq, births_before,
            "a recognized changed-context move is not a fresh allocation"
        );
        assert_projection(&wd, ident, rolls, &st);
        assert_eq!(
            feline(&wd).genome,
            g1,
            "ContextMove preserves the frozen genome"
        );
        assert_eq!(
            feline(&wd).appeared,
            t0,
            "ContextMove preserves the episode clock"
        );
        assert_eq!(
            wd.persist[&ident].ctx_fp, candidate_ctx,
            "recognition adopts the current context for later matching/done marks"
        );
        assert!(
            wd.persist[&ident].nova_done,
            "ContextMove preserves the spent guard"
        );
        assert!(m.check_invariant("RecognitionComplete", &st));
        assert!(m.check_invariant("NoFalseBirths", &st));
        assert!(m.check_invariant("PlayedOnce", &st));

        // Vanish: one blank rescan — the entry enters grace, nothing is lost.
        wd.rescan(&blank, 2, 48, &lex, &c, 4, at(3));
        assert!(m.fire("Vanish", &mut st));
        assert_projection(&wd, ident, rolls, &st);

        // Re-hit within grace: no roll, same genome, same appeared, nova spent.
        wd.rescan(&context_moved, 2, 48, &lex, &c, 5, at(4));
        assert!(m.fire("Rehit", &mut st));
        assert_projection(&wd, ident, rolls, &st);
        assert_eq!(feline(&wd).genome, g1);
        assert_eq!(
            feline(&wd).appeared,
            t0,
            "grace re-hit continues the episode (B-3)"
        );
        assert!(m.check_invariant("GenomeFrozen", &st));

        // Vanish again, then grace expiry: 18 s unseen > 10 s TTL ⇒ swept.
        wd.rescan(&blank, 2, 48, &lex, &c, 6, at(5));
        assert!(m.fire("Vanish", &mut st));
        wd.rescan(&blank, 2, 48, &lex, &c, 7, at(21));
        for _ in 0..3 {
            assert!(m.fire("Tick", &mut st));
        }
        assert!(m.fire("Expire", &mut st));
        assert!(wd.persist.is_empty(), "true identity death");
        assert_projection(&wd, ident, rolls, &st);

        // v3 §1.3: the re-appearance from the done set is a new episode (fresh
        // roll) born done: `PlayedOnce` holds and Ignite stays disabled.
        wd.rescan(&context_moved, 2, 48, &lex, &c, 8, at(22));
        rolls += 1;
        assert!(m.fire("Appear", &mut st));
        assert_projection(&wd, ident, rolls, &st);
        assert_eq!(feline(&wd).appeared, at(22));
        assert!(
            wd.persist[&ident].born_done,
            "the identity is done-marked: the re-birth is born-done"
        );
        assert!(
            !m.action_enabled("Ignite", &st),
            "the model's done flag survives Expire — no second Ignite"
        );
        assert!(m.check_invariant("PlayedOnce", &st));
        assert!(m.check_invariant("GenomeFrozen", &st));
    }

    /// ContextMove's soundness complement: a `kitty` retyped after nonblank
    /// partial-token frames, with equal FormId, is a fresh, armed episode,
    /// never the spent one transferred, in all three shapes:
    ///
    /// * at a changed column (no exact evidence at all);
    /// * at the same key after several partial scans (`TypedRetype`: the
    ///   exact context returns, but the taint forbids the transfer);
    /// * at the same key after ONE coalesced partial inside the two-scan weak
    ///   window (`RecentTypedRetype`: neither the same-row fast path nor an
    ///   alignment edge may take it).
    #[test]
    fn a_kitty_retyped_after_partial_frames_is_fresh_and_armed() {
        let model = aterm_models_effects::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let changed: &[&[u8]] = &[
            b"\x1b[2;1Hstatus a noisy k purrs",
            b"\x1b[2;1Hstatus a noisy ki purrs",
            b"\x1b[2;1Hstatus a noisy kitt purrs",
        ];
        let same: &[&[u8]] = &[
            b"old alpha k tail",
            b"old alpha ki tail",
            b"old alpha kitt tail",
        ];
        let one: &[&[u8]] = &[b"old alpha kitt tail"];
        for (label, partials, complete, action) in [
            (
                "changed column",
                changed,
                b"\x1b[2;1Hstatus a noisy kitty purrs".as_slice(),
                None,
            ),
            (
                "same key after more typing",
                same,
                b"old alpha kitty tail".as_slice(),
                Some("TypedRetype"),
            ),
            (
                "same key after one partial",
                one,
                b"old alpha kitty tail".as_slice(),
                Some("RecentTypedRetype"),
            ),
        ] {
            let t0 = Instant::now();
            let at = |n: usize| t0 + Duration::from_millis(16 * n as u64);
            let mut original = Terminal::new(4, 48);
            original.process(b"old alpha kitty tail");
            let mut wd = WordDecorations::default();
            wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
            let old = feline(&wd).clone();
            let old_ctx = wd.persist[&old.ident].ctx_fp;
            wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

            for (i, partial) in partials.iter().enumerate() {
                let mut typing = Terminal::new(4, 48);
                typing.process(partial);
                wd.rescan(&typing, 4, 48, &lex, &c, i as u64 + 2, at(i + 1));
                assert!(
                    wd.occ.iter().all(|o| o.class != Class::Feline),
                    "{label}: partial token {i} must not be recognized"
                );
            }
            if action.is_some() {
                let tainted = wd.persist[&old.ident];
                assert!(
                    tainted.continuity_tainted,
                    "{label}: PRECONDITION — nonblank partial scans taint the old episode"
                );
                if partials.len() == 1 {
                    assert_eq!(
                        wd.rescan_seq.wrapping_sub(tainted.seen_seq),
                        1,
                        "{label}: PRECONDITION — inside the weak-continuity window"
                    );
                }
            }

            let births_before = wd.birth_seq;
            let appeared = at(partials.len() + 1);
            let mut replacement = Terminal::new(4, 48);
            replacement.process(complete);
            let seq = partials.len() as u64 + 2;
            wd.rescan(&replacement, 4, 48, &lex, &c, seq, appeared);
            let new = feline(&wd);
            assert_eq!(
                new.form_id, old.form_id,
                "{label}: the exact surface is kitty"
            );
            assert_eq!(
                new.ident == old.ident,
                action.is_some(),
                "{label}: PRECONDITION — the fixture's identity key"
            );
            if action.is_some() {
                assert_eq!(
                    wd.persist[&new.ident].ctx_fp, old_ctx,
                    "{label}: PRECONDITION — the exact context returns"
                );
            }
            assert_eq!(
                wd.birth_seq,
                births_before + 1,
                "{label}: one genuine new birth"
            );
            assert_eq!(
                new.appeared, appeared,
                "{label}: the new kitty activates now"
            );
            assert!(
                !wd.persist[&new.ident].nova_done,
                "{label}: the retyped kitty is armed rather than inheriting spent state"
            );
            if let Some(action) = action {
                let mut state = model.init_state();
                assert!(model.fire(action, &mut state), "{label}");
                assert_eq!(
                    state[&"fresh"], 1,
                    "{label}: real birth projects onto {action}"
                );
                assert_eq!(state[&"armed"], 1, "{label}");
                assert_eq!(state[&"transferred"], 0, "{label}");
                assert!(model.check_invariant("NoFalseTransfers", &state), "{label}");
            }
        }
    }

    /// The episode replaced by one explicit kitty retype does not make the next
    /// retype born-done; each cycle ends with the caret one past the `y`.
    #[test]
    fn two_consecutive_recent_kitty_retypes_both_arm() {
        let model = aterm_models_effects::sparkle_retype_rearm_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut complete = Terminal::new(4, 48);
        complete.process(b"old alpha kitty");
        let mut partial = Terminal::new(4, 48);
        partial.process(b"old alpha kitt");

        let mut wd = WordDecorations::default();
        wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;
        // Precondition: the caret is one cell past the token (the witness).
        {
            let occ = feline(&wd);
            assert_eq!(
                (complete.cursor().row, complete.cursor().col),
                (occ.row, occ.end_col + 1),
                "the fixture must stage a caret-witnessed completion"
            );
        }
        wd.persist.get_mut(&ident).expect("initial kitty").nova_done = true;
        let births_before = wd.birth_seq;
        let mut state = model.init_state();

        for cycle in 1..=2u64 {
            let partial_seq = cycle * 2;
            wd.rescan(
                &partial,
                4,
                48,
                &lex,
                &c,
                partial_seq,
                t0 + Duration::from_millis(16 * (partial_seq - 1)),
            );
            assert!(wd.persist[&ident].continuity_tainted, "cycle {cycle}");

            let complete_seq = partial_seq + 1;
            let appeared = t0 + Duration::from_millis(16 * (complete_seq - 1));
            // One keystroke per retype: the witness is spent on use.
            wd.note_typed_edit(appeared, None);
            wd.rescan(&complete, 4, 48, &lex, &c, complete_seq, appeared);
            assert!(model.fire("TypeAgain", &mut state));
            let current = feline(&wd);
            assert_eq!(current.ident, ident);
            assert_eq!(current.appeared, appeared, "cycle {cycle} is fresh");
            assert_eq!(
                wd.birth_seq - births_before,
                state[&"armed"] as u64,
                "every completed retype is armed"
            );
            assert!(!current.inert, "cycle {cycle} must not be born-done");
            assert!(!wd.persist[&ident].nova_done, "cycle {cycle} is armed");
            let done_key = ident ^ wd.persist[&ident].ctx_fp;
            assert!(
                !wd.done_marks.contains_key(&done_key),
                "cycle {cycle} leaves no typed-reentry poison mark"
            );

            // Model a completed one-shot before the next intentional retype.
            wd.persist.get_mut(&ident).expect("live kitty").nova_done = true;
        }
        assert_eq!(state[&"retypes"], 2);
        assert_eq!(state[&"armed"], 2);
        assert!(model.check_invariant("EveryRetypeArmed", &state));
    }

    /// One keystroke re-arms at most one cat however many repaints follow: in
    /// one witness window, cycle 1 rides its key and arms, cycle 2 (no key)
    /// stays inert, cycle 3 brings a new key and arms.
    #[test]
    fn one_keystroke_re_arms_one_cat_however_many_repaints_follow() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut complete = Terminal::new(4, 48);
        complete.process(b"old alpha kitty");
        let mut partial = Terminal::new(4, 48);
        partial.process(b"old alpha kitt");

        let mut wd = WordDecorations::default();
        wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;
        assert!(
            !feline(&wd).inert,
            "PRECONDITION: the first typed kitty is armed"
        );

        // Each cycle: spend the one-shot, blank the word, restore it under the
        // caret (a repaint loop); only `key` varies.
        let mut out = Vec::new();
        let mut cycle = |wd: &mut WordDecorations, seq: u64, base_ms: u64, key: bool| -> bool {
            wd.persist.get_mut(&ident).expect("live kitty").nova_done = true;
            // The tick stamps the ledger (`stamp_spent_marks`), the guard a
            // re-arm must overcome.
            tick_deco(wd, at(base_ms), &c, &mut out);
            let done_key = ident ^ wd.persist[&ident].ctx_fp;
            assert!(
                wd.done_marks.contains_key(&done_key),
                "PRECONDITION: cycle at {base_ms} ms starts from a stamped \
                 guard, so an armed verdict can only come from a re-arm"
            );
            wd.rescan(&partial, 4, 48, &lex, &c, seq, at(base_ms + 4));
            assert!(
                wd.persist[&ident].continuity_tainted,
                "PRECONDITION: cycle at {base_ms} ms must taint, or it is not \
                 exercising the disputed branch"
            );
            if key {
                wd.note_typed_edit(at(base_ms + 8), None);
            }
            wd.rescan(&complete, 4, 48, &lex, &c, seq + 1, at(base_ms + 16));
            !feline(wd).inert
        };

        assert!(
            cycle(&mut wd, 2, 100, true),
            "cycle 1: a key was committed, so the retype re-arms"
        );
        assert!(
            !cycle(&mut wd, 4, 200, false),
            "cycle 2: SAME key, SAME window, no new keystroke — the repaint \
             must not ride it a second time"
        );
        assert!(
            at(316).duration_since(at(108)) < Duration::from_millis(TYPED_EDIT_WITNESS_MS),
            "PRECONDITION: cycle 2 really is inside cycle 1's witness window, \
             so its silence is the SPEND and not the clock"
        );
        assert!(
            cycle(&mut wd, 6, 300, true),
            "cycle 3: a fresh key re-arms again — the spend retires the KEY, \
             not the feature"
        );
    }

    /// A redraw during the rise or dwell does not replay the cat: the mark is
    /// written when the one-shot starts.
    #[test]
    fn a_repaint_midway_through_the_cat_plays_no_second_cat() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        let t0 = Instant::now();
        let mut typed = Terminal::new(g.rows, g.cols);
        typed.process(b"hello kitty");
        let mut overwritten = Terminal::new(g.rows, g.cols);
        overwritten.process(b"hello ------");
        let mut restored_elsewhere = Terminal::new(g.rows, g.cols);
        restored_elsewhere.process(b"hello kitty\r\n\r\n$ ");

        let mut wd = WordDecorations::default();
        wd.rescan(&typed, rows, cols, &lex, &c, 1, t0);
        let first = feline(&wd).clone();
        assert!(!first.inert, "PRECONDITION: the typed kitty is armed");

        // One frame: the cat is on glass and rising, nowhere near done.
        let mid = t0 + Duration::from_millis(60);
        let (free, _, _) = tick_cat(&mut wd, mid, &c, g);
        assert!(
            !free.is_empty(),
            "PRECONDITION: the cat is actually drawing"
        );
        assert!(
            wd.persist[&first.ident].peek_started && !wd.persist[&first.ident].peek_done,
            "PRECONDITION: mid-animation — started, NOT finished"
        );
        assert!(
            wd.done_marks
                .contains_key(&(first.ident ^ wd.persist[&first.ident].ctx_fp)),
            "a STARTED one-shot is already guarded, not only a finished one"
        );

        wd.rescan(
            &overwritten,
            rows,
            cols,
            &lex,
            &c,
            2,
            mid + Duration::from_millis(16),
        );
        assert!(
            wd.persist[&first.ident].continuity_tainted,
            "PRECONDITION: the overwrite tainted the in-flight episode"
        );
        wd.rescan(
            &restored_elsewhere,
            rows,
            cols,
            &lex,
            &c,
            3,
            mid + Duration::from_millis(32),
        );
        assert!(
            feline(&wd).inert,
            "a repaint mid-cat must not restart the entrance"
        );
    }

    /// At a full persist map, an unmatched old kitty offered back for grace
    /// after a fresh move loses the capacity race (no entry 513); the fresh
    /// kitty stays armed and resident.
    #[test]
    fn persist_cap_drops_unmatched_grace_after_fresh_move() {
        let model = aterm_models_effects::sparkle_persist_capacity_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let mut replacement = Terminal::new(4, 48);
        replacement.process(b"status beta kitty tail");

        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        wd.persist.get_mut(&old.ident).expect("old kitty").nova_done = true;
        let old_ep = wd.persist[&old.ident];

        // Resolve the replacement key independently so the synthetic resident
        // keys cannot accidentally claim either side of the real rekey.
        let mut probe = WordDecorations::default();
        probe.rescan(&replacement, 4, 48, &lex, &c, 1, t0);
        let replacement_ident = feline(&probe).ident;
        assert_ne!(replacement_ident, old.ident, "fixture changes the column");

        let mut synthetic = 0u64;
        while wd.persist.len() < PERSIST_CAP {
            let key = 0xC0DE_0000_0000_0000u64.wrapping_add(synthetic);
            synthetic += 1;
            if key == old.ident || key == replacement_ident {
                continue;
            }
            let mut ep = old_ep;
            ep.form_id = FormId::UNKNOWN;
            ep.seed = key;
            ep.ctx_fp = key.rotate_left(17);
            ep.last_seen = t0 + Duration::from_millis(1);
            ep.last_row = 3;
            ep.last_col = 0;
            ep.nova_done = false;
            wd.persist.insert(key, ep);
        }
        assert_eq!(wd.persist.len(), PERSIST_CAP);

        // Blank scans past the weak-continuity window leave the old episode
        // untainted; the changed context and column defeat both exact arms,
        // so this exercises the ordinary unmatched-grace admission.
        for i in 0..3 {
            let blank = Terminal::new(4, 48);
            wd.rescan(
                &blank,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
            assert_eq!(wd.persist.len(), PERSIST_CAP);
        }
        assert!(!wd.persist[&old.ident].continuity_tainted);

        let mut state = model.init_state();
        for action in ["Pull", "Fresh", "Reinsert"] {
            assert!(model.fire(action, &mut state));
        }
        let births_before = wd.birth_seq;
        let appeared = t0 + Duration::from_millis(64);
        wd.rescan(&replacement, 4, 48, &lex, &c, 5, appeared);
        let new = feline(&wd);
        assert_eq!(new.ident, replacement_ident);
        assert_eq!(new.appeared, appeared, "replacement kitty is freshly armed");
        assert_eq!(wd.birth_seq, births_before + 1);
        assert_eq!(state[&"resident"], 3);
        assert_eq!(state[&"admitted"], 1);
        assert_eq!(state[&"departed"], 1);
        assert_eq!(
            wd.persist.len(),
            PERSIST_CAP * state[&"resident"] as usize / 3,
            "the real full-map transaction projects onto ResidentBounded"
        );
        assert!(wd.persist.contains_key(&replacement_ident));
        assert!(!wd.persist.contains_key(&old.ident));
        let old_marked = wd.done_marks.contains_key(&(old.ident ^ old_ep.ctx_fp));
        assert_eq!(
            i64::from(old_marked),
            state[&"departed"],
            "dropping grace history preserves its spent one-shot guard"
        );

        // A subsequent ordinary damage scan remains within the bound and
        // traverses the same fixed-size alignment scratch safely.
        wd.rescan(
            &replacement,
            4,
            48,
            &lex,
            &c,
            6,
            appeared + Duration::from_millis(16),
        );
        assert_eq!(wd.persist.len(), PERSIST_CAP);
    }

    /// The redraw windows of a spent `kitty` that went away and came back.
    /// Each row is one boundary of the transfer rules:
    ///
    /// * the weak redraw window is exactly two scans: a changed-context redraw
    ///   at gap 2 transfers the episode, at gap 3 it is a fresh birth;
    /// * blank occlusion is grace, not a redraw: the exact context returning
    ///   after MORE than two blank scans still transfers (`BlankGrace`);
    /// * TTL expiry happens before adoption, so an hour-old episode is never
    ///   resurrected by a low rescan count, through alignment or through the
    ///   same-position fast path (where the departure's done mark makes the
    ///   rebirth born-done);
    /// * a width change never takes the weak redraw transfer.
    #[test]
    fn a_returning_spent_kitty_transfers_only_inside_its_redraw_windows() {
        let model = aterm_models_effects::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let moved_at = |cols: u16| {
            let mut t = Terminal::new(4, cols);
            t.process(b"\x1b[2;1Hstatus a noisy kitty purrs");
            t
        };
        let (moved, moved_wider) = (moved_at(48), moved_at(49));
        let blank = Terminal::new(4, 48);
        /// What the returning occurrence must be.
        #[derive(Clone, Copy, Debug, PartialEq)]
        enum Verdict {
            /// The spent episode transferred (clock and guard kept).
            Transferred,
            /// A fresh birth, armed.
            Armed,
            /// A fresh birth, born-done from the departure's mark.
            BornDone,
            /// A fresh birth, nothing adopted (settling may make it inert).
            Fresh,
        }
        for (label, blanks, ret_ms, target, cols, verdict, action) in [
            (
                "gap 2",
                &[16u64][..],
                32,
                &moved,
                48,
                Verdict::Transferred,
                None,
            ),
            ("gap 3", &[16, 32][..], 48, &moved, 48, Verdict::Armed, None),
            (
                "three blank scans, exact context",
                &[1_000, 2_000, 3_000][..],
                4_000,
                &original,
                48,
                Verdict::Transferred,
                Some("BlankGrace"),
            ),
            (
                "hour old, moved",
                &[1_000][..],
                3_600_000,
                &moved,
                48,
                Verdict::Armed,
                None,
            ),
            (
                "hour old, same position",
                &[1_000][..],
                3_600_000,
                &original,
                48,
                Verdict::BornDone,
                None,
            ),
            (
                "width change",
                &[][..],
                16,
                &moved_wider,
                49,
                Verdict::Fresh,
                None,
            ),
        ] {
            let t0 = Instant::now();
            let at = |ms: u64| t0 + Duration::from_millis(ms);
            let mut wd = WordDecorations::default();
            wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
            let old = feline(&wd).clone();
            let old_ctx = wd.persist[&old.ident].ctx_fp;
            wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;
            for (n, &ms) in blanks.iter().enumerate() {
                wd.rescan(&blank, 4, 48, &lex, &c, n as u64 + 2, at(ms));
            }
            let births_before = wd.birth_seq;
            let seq = blanks.len() as u64 + 2;
            wd.rescan(target, 4, cols, &lex, &c, seq, at(ret_ms));
            let back = feline(&wd);
            let ep = &wd.persist[&back.ident];
            let fresh = wd.birth_seq - births_before;
            if verdict == Verdict::Transferred {
                assert_eq!(fresh, 0, "{label}: the episode transfers");
                assert_eq!(back.appeared, t0, "{label}: the episode clock survives");
                assert!(ep.nova_done, "{label}: the spent guard survives");
            } else {
                assert_eq!(fresh, 1, "{label}: a fresh birth, nothing adopted");
                assert_eq!(back.appeared, at(ret_ms), "{label}");
            }
            match verdict {
                Verdict::Armed => assert!(!ep.nova_done, "{label}: armed"),
                Verdict::BornDone => assert!(
                    ep.born_done,
                    "{label}: true death records the one-shot before the identity returns"
                ),
                Verdict::Transferred | Verdict::Fresh => {}
            }
            if let Some(action) = action {
                assert!(
                    !ep.continuity_tainted,
                    "{label}: PRECONDITION — blank occlusion never taints"
                );
                assert_eq!(ep.ctx_fp, old_ctx, "{label}: the exact context returns");
                let mut state = model.init_state();
                assert!(model.fire(action, &mut state), "{label}");
                assert_eq!(
                    fresh, state[&"fresh"] as u64,
                    "{label}: projects onto {action}"
                );
                assert_eq!(state[&"transferred"], 1, "{label}");
                assert_eq!(state[&"armed"], 0, "{label}");
                assert!(
                    model.check_invariant("RecognitionComplete", &state),
                    "{label}"
                );
            }
        }
    }

    /// Tier-1 binding for `SparkleReflowCardinality`: two repeated surfaces
    /// survive a context-changing 2→2 redraw with no births, and 2→3 births
    /// exactly one (association checked as a multiset).
    #[test]
    fn sparkle_reflow_cardinality_conformance_repeated_surfaces() {
        let model = aterm_models_effects::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let (original, moved, grown) = repeated_kitty_reflow_frames();
        let mut wd = WordDecorations::default();

        wd.rescan(&original, 6, 48, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 2, "the old group has two equal surfaces");
        assert_eq!(wd.birth_seq, 2);
        let form = wd.occ[0].form_id;
        assert!(wd.occ.iter().all(|o| o.form_id == form));
        let old_contexts: Vec<u64> = wd.occ.iter().map(|o| wd.persist[&o.ident].ctx_fp).collect();
        for o in &wd.occ {
            wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
        }

        // Both fingerprints changed, so the pass cannot take the exact-context
        // arm.
        let mut probe = WordDecorations::default();
        probe.rescan(&moved, 6, 48, &lex, &c, 1, t0);
        assert_eq!(probe.occ.len(), 2);
        let moved_contexts: Vec<(u64, u64)> = probe
            .occ
            .iter()
            .map(|o| (o.ident, probe.persist[&o.ident].ctx_fp))
            .collect();
        assert!(
            moved_contexts
                .iter()
                .all(|(_, ctx)| !old_contexts.contains(ctx)),
            "each repeated-surface candidate has genuinely changed context"
        );

        let births_before = wd.birth_seq;
        wd.rescan(&moved, 6, 48, &lex, &c, 2, at(1));
        let mut pair = model.init_state();
        assert!(model.fire("MovePair", &mut pair));
        assert_eq!(
            wd.birth_seq - births_before,
            pair[&"fresh"] as u64,
            "2→2 births project onto the model"
        );
        assert_eq!(wd.occ.len() as i64, pair[&"new_count"]);
        assert_eq!(wd.persist.len(), 2, "no unmatched stale episode remains");
        let armed = wd
            .occ
            .iter()
            .filter(|o| !wd.persist[&o.ident].nova_done)
            .count() as i64;
        assert_eq!(armed, pair[&"armed"]);
        for o in &wd.occ {
            let ep = &wd.persist[&o.ident];
            let expected_ctx = moved_contexts
                .iter()
                .find_map(|(ident, ctx)| (*ident == o.ident).then_some(*ctx))
                .expect("probe candidate");
            assert_eq!(ep.ctx_fp, expected_ctx, "recognition context advances");
            assert_eq!(ep.appeared, t0, "the survivor clock is preserved");
            assert!(ep.nova_done, "every 2→2 survivor remains spent");
        }
        assert!(model.check_invariant("NoFalseBirths", &pair));
        assert!(model.check_invariant("FreshAtMostNetGrowth", &pair));

        let births_before = wd.birth_seq;
        wd.rescan(&grown, 6, 48, &lex, &c, 3, at(2));
        let mut growth = model.init_state();
        assert!(model.fire("GrowOne", &mut growth));
        assert_eq!(
            wd.birth_seq - births_before,
            growth[&"fresh"] as u64,
            "2→3 births project onto net growth"
        );
        assert_eq!(wd.occ.len() as i64, growth[&"new_count"]);
        assert_eq!(wd.persist.len(), 3);
        let old_spent = wd
            .occ
            .iter()
            .filter(|o| {
                let ep = &wd.persist[&o.ident];
                ep.appeared == t0 && ep.nova_done
            })
            .count() as i64;
        let fresh_armed = wd
            .occ
            .iter()
            .filter(|o| {
                let ep = &wd.persist[&o.ident];
                ep.appeared == at(2) && !ep.nova_done
            })
            .count() as i64;
        assert_eq!(old_spent, growth[&"transferred"]);
        assert_eq!(fresh_armed, growth[&"armed"]);
        assert!(model.check_invariant("FreshAtMostNetGrowth", &growth));
        assert!(model.check_invariant("ArmedAtMostNetGrowth", &growth));
        assert!(model.check_invariant("NoFalseBirths", &growth));
    }

    /// Negative control: clearing the transferable old group makes 2→2 arm two
    /// and 2→3 arm three, matching `Buggy=1`.
    #[test]
    fn sparkle_reflow_cardinality_negative_control_context_gate() {
        let mut buggy = aterm_models_effects::sparkle_reflow_cardinality_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let (original, moved, grown) = repeated_kitty_reflow_frames();

        let run = |next: &Terminal, action: &str| {
            let mut wd = WordDecorations::default();
            wd.rescan(&original, 6, 48, &lex, &c, 1, t0);
            assert_eq!(wd.birth_seq, 2);
            for o in &wd.occ {
                wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
            }
            wd.persist.clear();
            wd.done_marks.clear();
            let births_before = wd.birth_seq;
            wd.rescan(next, 6, 48, &lex, &c, 2, t0 + Duration::from_secs(1));

            let mut st = buggy.init_state();
            assert!(buggy.fire(action, &mut st));
            assert_eq!(wd.occ.len() as i64, st[&"new_count"]);
            assert_eq!(wd.birth_seq - births_before, st[&"fresh"] as u64);
            let armed = wd
                .occ
                .iter()
                .filter(|o| !wd.persist[&o.ident].nova_done)
                .count() as i64;
            assert_eq!(armed, st[&"armed"]);
            assert!(!buggy.check_invariant("FreshAtMostNetGrowth", &st));
            assert!(!buggy.check_invariant("ArmedAtMostNetGrowth", &st));
            assert!(!buggy.check_invariant("NoFalseBirths", &st));
        };

        run(&moved, "MovePair");
        run(&grown, "GrowOne");
    }

    /// Negative control: a grace-less map (TTL zero) reproduces `Buggy = 1`:
    /// the re-hit re-rolls and the spent-nova guard is lost.
    #[test]
    fn sparkle_identity_negative_control_graceless_map_is_buggy_trace() {
        let healthy = aterm_models_effects::sparkle_identity_model();
        let mut buggy = aterm_models_effects::sparkle_identity_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut st = buggy.init_state();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations {
            grace_override: Some(Duration::ZERO),
            ..WordDecorations::default()
        };
        let mut term = Terminal::new(2, 48);
        term.process(b"a fluffy kitty naps");
        let blank = Terminal::new(2, 48);
        let mut rolls = 0i64;

        wd.rescan(&term, 2, 48, &lex, &c, 1, t0);
        rolls += 1;
        assert!(buggy.fire("Appear", &mut st));
        let ident = mix(feline(&wd).seed);
        wd.persist.get_mut(&ident).expect("live").nova_done = true;
        assert!(buggy.fire("Ignite", &mut st));

        // One blank rescan: the grace-less map forgets the episode instantly.
        wd.rescan(&blank, 2, 48, &lex, &c, 2, t0 + Duration::from_secs(1));
        assert!(buggy.fire("Vanish", &mut st));
        assert!(
            wd.persist.is_empty(),
            "grace-less: the episode dies with one epoch"
        );

        // The re-hit is a fresh miss and re-rolls, as Buggy's Rehit does. Done
        // marks would mask it (born-done), so this mark-less control drops them.
        wd.done_marks.clear();
        wd.rescan(&term, 2, 48, &lex, &c, 3, t0 + Duration::from_secs(2));
        rolls += 1;
        assert!(buggy.fire("Rehit", &mut st));
        assert_eq!(st[&"rolls"], 2, "Buggy Rehit re-rolls");
        assert_eq!(
            rolls, st[&"rolls"],
            "the grace-less map reproduces the Buggy trace"
        );
        assert!(
            !wd.persist[&ident].nova_done,
            "the spent-nova guard is lost — the strobe hole"
        );
        // The healthy model rejects this state: GenomeFrozen is violated.
        assert!(
            !healthy.check_invariant("GenomeFrozen", &st),
            "births=1 rolls=2 must violate GenomeFrozen"
        );
        // The lost guard admits a second ignition: `OneNovaPerEpisode` fails.
        assert!(buggy.fire("Ignite", &mut st));
        assert!(!healthy.check_invariant("OneNovaPerEpisode", &st));
    }

    // ───────────────────── §5 peeking-cat battery ─────────────────────

    /// 10×20 reference cell over a 6×20 grid (§7's reference geometry class).
    fn geom20() -> EffectGeom {
        EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 6,
            cols: 20,
        }
    }

    /// Scan a terminal through the geometry-aware snapshot path (the shipping
    /// one) with the live caret at `caret`, and hand back the engine.
    fn scan_grid(
        term: &mut Terminal,
        c: &DecoConfig,
        geom: EffectGeom,
        now: Instant,
        caret: Option<(u16, u16)>,
    ) -> WordDecorations {
        let (rows, cols) = (usize::from(geom.rows), usize::from(geom.cols));
        let lex = lex();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        // Snapshot rows may arrive trimmed: take the bg from the first cell.
        let bg = snap
            .cells
            .iter()
            .find_map(|line| line.first())
            .map_or(0, |cell| rgb3_to_u32(cell.bg));
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            c,
            1,
            now,
            geom,
            bg,
            caret,
        );
        wd
    }

    /// A `kitty` inside a TUI prompt box (`╭──╮` above, `│` beside) still gets
    /// a cat: the frame draws over the fur.
    #[test]
    fn tui_prompt_box_frame_still_summons_a_cat() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        // A typical TUI prompt: a boxed input line.
        term.process(
            "\r\n\r\n╭──────────────────╮\r\n│ kitty            │\r\n╰──────────────────╯"
                .as_bytes(),
        );
        let mut wd = scan_grid(&mut term, &c, g, t0, None);
        let occ = feline(&wd);
        assert!(
            occ.cat_text_clear && cat_eligible(occ, &c, g),
            "a boxed prompt line is eligible — the frame draws OVER the fur"
        );
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
        assert!(
            !cats.is_empty(),
            "typing `kitty` inside a TUI prompt box summons a cat"
        );
    }

    /// Words that arrived while the window was not presenting (no rescan ran)
    /// are spent on return, not all played at once.
    #[test]
    fn words_arriving_while_away_never_storm_on_return() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let lex = lex();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut wd = WordDecorations::default();

        // Focused and idle: nothing on screen yet.
        wd.set_presentable(t0, true);
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

        // Away: no rescan runs while feline words scroll past.
        wd.set_presentable(t0 + Duration::from_secs(1), false);
        term.process(b"kitty\r\nkitty\r\nkitty\r\nkitty");

        // …and comes back two minutes later. This is the first rescan since.
        let back = t0 + Duration::from_secs(120);
        wd.set_presentable(back, true);
        wd.rescan(&term, rows, cols, &lex, &c, 2, back);
        let felines = wd.occ.iter().filter(|o| o.spec.graphic.is_some()).count();
        assert!(felines >= 3, "the fixture really does scan several cats");
        let (cats, _, _) = tick_cat(&mut wd, back + Duration::from_millis(600), &c, g);
        assert!(
            cats.is_empty(),
            "returning to a screen of feline words plays NO entrances, got {}",
            cats.len()
        );
        for occ in wd.occ.iter().filter(|o| o.spec.graphic.is_some()) {
            assert!(
                wd.persist[&occ.ident].born_settled,
                "each word that arrived while away is born spent"
            );
        }

        // Only the first rescan back is spent: typing after refocus earns a cat.
        term.process(b"\r\nkitty");
        let after = back + Duration::from_secs(1);
        wd.rescan(&term, rows, cols, &lex, &c, 3, after);
        let fresh = wd
            .occ
            .iter()
            .filter(|o| o.spec.graphic.is_some())
            .filter(|o| !wd.persist[&o.ident].born_settled)
            .count();
        assert_eq!(fresh, 1, "the newly typed word is armed normally");
    }

    /// No timer re-arm: a spent feline word left on screen (rescanned for half
    /// an hour) never plays again without a keystroke.
    #[test]
    fn a_settled_kitty_on_screen_never_plays_again_without_a_keystroke() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let lex = lex();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\r\n\r\nnice kitty");
        let mut wd = WordDecorations::default();
        wd.set_presentable(t0, true);
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;

        // Precondition: the word's one cat really plays, and really ends.
        let (first, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(100), &c, g);
        assert!(!first.is_empty(), "PRECONDITION: the first cat draws");
        let spent = t0 + Duration::from_secs(8);
        tick_cat(&mut wd, spent, &c, g);
        assert!(
            wd.persist[&ident].peek_done,
            "PRECONDITION: the one-shot is spent before the watch begins"
        );
        let done_key = ident ^ wd.persist[&ident].ctx_fp;

        // Half an hour of an unchanged grid redrawn twice a second, each redraw
        // bumping the damage epoch.
        let mut drew_again = Vec::new();
        for i in 0..3600u64 {
            let at = spent + Duration::from_millis(500 * (i + 1));
            wd.rescan(&term, rows, cols, &lex, &c, 2 + i, at);
            let (cats, _, _) = tick_cat(&mut wd, at + Duration::from_millis(60), &c, g);
            if !cats.is_empty() {
                drew_again.push(at.saturating_duration_since(t0));
            }
        }
        assert!(
            drew_again.is_empty(),
            "a word nobody retyped played its cat again at {drew_again:?} — \
             that is the owner's 'new kitties appear' on a longer timer"
        );
        assert!(
            wd.persist[&ident].peek_done,
            "the settled episode is never re-armed"
        );
        assert!(
            wd.done_marks.contains_key(&done_key),
            "…and its ledger guard is never deleted out from under it"
        );
    }

    /// With the rows above walled and below clear, an interior word peeks down.
    #[test]
    fn busy_band_above_flips_the_peek_downward() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"XXXXXXXXXXXXXXXXXXXX\r\nXXXXXXXXXXXXXXXXXXXX\r\nkitty");
        let wd = scan_grid(&mut term, &c, g, t0, None);
        let occ = feline(&wd);
        assert_eq!(occ.row, 2);
        assert!(
            occ.cat_text_clear,
            "a busy band above no longer erases the cat"
        );
        assert!(occ.cat_peek_down, "it takes the clear side instead");
    }

    /// A HEAD NEVER LANDS ON LETTERS: a word on the bottom row (no room
    /// below) under a line of text that runs through where its head would
    /// rise gets no head (ink only), as the fox an Enter at the bottom of a
    /// full screen printed did, its head over the echoed command's letters
    /// (1.4:1). Negative controls: the same word with the line above blank
    /// peeks up; a frame's box-drawing lines there still host it.
    #[test]
    fn a_head_never_lands_on_letters() {
        let (rows, cols) = (4usize, 20usize);
        let c = cfg();
        let g = EffectGeom {
            rows: rows as u16,
            ..geom20()
        };
        let t0 = Instant::now();
        let plan = |above: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            let mut bytes = b"\r\n\r\n".to_vec();
            bytes.extend_from_slice(above);
            bytes.extend_from_slice(b"\r\nsee my kitty");
            term.process(&bytes);
            let wd = scan_grid(&mut term, &c, g, t0, None);
            let occ = feline(&wd);
            assert_eq!(occ.row, 3, "fixture: the word is on the bottom row");
            (occ.cat_text_clear, occ.cat_peek_down)
        };
        assert_eq!(
            plan(b"the quick brown fox jumps"),
            (false, false),
            "a head rose over the letters above"
        );
        assert_eq!(
            plan(b""),
            (true, false),
            "control: a blank line above hosts it"
        );
        assert_eq!(
            plan("\u{2500}".repeat(cols).as_bytes()),
            (true, false),
            "control: a frame's lines above host it"
        );
    }

    /// A feline occurrence with a hand-picked ordinary genome; idents keyed off
    /// (row, start) so multi-cat tests get distinct identities.
    fn cat_occ(row: u16, start: u16, end: u16, gkey: u64, now: Instant) -> Occurrence {
        Occurrence {
            row,
            start_col: start,
            end_col: end,
            class: Class::Feline,
            langs: LangSet::EMPTY,
            form_id: FormId::UNKNOWN,
            seed: 0xCA7,
            ident: mix(0xCA7 ^ (u64::from(row) << 32) ^ u64::from(start)),
            appeared: now,
            // The magic word decodes bare: no magic build, no accessory, so no
            // dwell bonus.
            genome: Genome {
                gkey,
                magic: (200 << 24) | 100,
            },
            ink_base: 0,
            ink_cells: 0,
            ink_bg: [0; 3],
            cat_colors: CatColorKey::default(),
            cat_text_clear: true,
            cat_peek_down: false,
            dec_line: false,
            inert: false,
            spec: class_default_spec(Class::Feline, &cfg()),
            custom: false,
            species: None,
        }
    }

    /// A mid-dwell sample instant (from birth): past the rise and the kitten
    /// bounce, well before the shortest dwell ends.
    const DWELL_QUIET_MS: u64 = 950;

    /// gkey with age = Adult (Gray 0b11 @ v4 bits 13–14 → ord 2).
    const GKEY_ADULT_HEAD: u64 = 3 << 13;

    fn tick_cat(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
    ) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
        tick_cat_at(wd, now, cfg, geom, None, true)
    }

    /// [`tick_cat`] with an explicit companion cell and focus state; returns
    /// the free-sprite stream.
    fn tick_cat_at(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        companion_at: Option<(u16, u16)>,
        focused: bool,
    ) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
        tick_cat_companion(
            wd,
            now,
            cfg,
            geom,
            companion_at.map(CompanionOnGlass::at_cell),
            focused,
        )
    }

    /// [`tick_cat_at`] with the full companion view — the resident pet hands
    /// the engine its live body rect alongside the caret cell.
    fn tick_cat_companion(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        companion: Option<CompanionOnGlass>,
        focused: bool,
    ) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
        let mut out = Vec::new();
        let mut ink = Vec::new();
        let mut fr = Vec::new();
        let mut nova = Vec::new();
        let fp = wd.tick(
            now, cfg, geom, companion, None, focused, &mut out, &mut ink, &mut fr, &mut nova,
        );
        (fr, out, fp)
    }

    /// Incremental typing creates the episode when the final `y` lands, and
    /// the top-row entrance is visible on the first animation sample.
    /// A still, opaque, note-less cursor-companion frame at `cursor`.
    fn still_frame(geom: EffectGeom, cursor: (u16, u16)) -> KittyCursorFrame {
        KittyCursorFrame {
            geom,
            cursor,
            look: KittyLook::default(),
            colors: CatColorKey::default(),
            bob: 0.0,
            lift: 0.0,
            alpha: 255,
            pose: crate::kitty_cursor::CatPose::STILL,
            sing: 0.0,
            notes: [None; crate::kitty_sing::MAX_NOTES],
        }
    }

    /// The companion's layout at `cursor`: default look, no bob.
    fn still_layout(geom: EffectGeom, cursor: (u16, u16)) -> KittyCursorLayout {
        KittyCursorLayout {
            geom,
            cursor,
            look: KittyLook::default(),
            bob: 0.0,
            lift: 0.0,
        }
    }

    /// A settled (no fold) companion placement sample at `at`, facing right.
    fn settled_at(at: Instant) -> crate::kitty_cursor::CursorCatPlacementFrame {
        crate::kitty_cursor::CursorCatPlacementFrame {
            sampled_at: at,
            fold: None,
            facing_left: false,
        }
    }

    /// Resolve and emit one companion frame through the shipping placement
    /// path; the body is the last sprite it emits.
    fn emit_placed(
        wd: &mut WordDecorations,
        geom: EffectGeom,
        cursor: (u16, u16),
        motion: crate::kitty_cursor::CursorCatPlacementFrame,
    ) -> (KittyCursorPlacement, FreeSprite) {
        let placement = wd
            .resolve_kitty_cursor_placement(still_layout(geom, cursor), motion)
            .expect("placeable companion");
        let mut free = Vec::new();
        wd.kitty_cursor_at_placement(still_frame(geom, cursor), placement, &mut free)
            .expect("companion emitted");
        (placement, *free.last().expect("the body emits last"))
    }

    /// One host frame in `redraw_window` order: the engine tick, then the
    /// cursor companion's emission at `companion`, sharing one free stream.
    fn companion_frame(
        wd: &mut WordDecorations,
        now: Instant,
        c: &DecoConfig,
        g: EffectGeom,
        companion: Option<(u16, u16)>,
    ) -> Vec<FreeSprite> {
        let (mut free, _, _) = tick_cat_at(wd, now, c, g, companion, true);
        if let Some(cell) = companion {
            wd.kitty_cursor(still_frame(g, cell), &mut free);
        }
        free
    }

    /// The word-peek sprites of a free stream: word cats draw under the text,
    /// the companion over it.
    fn ambient(free: &[FreeSprite]) -> Vec<FreeSprite> {
        free.iter()
            .filter(|s| matches!(s.z, FreeZ::UnderText))
            .copied()
            .collect()
    }

    #[test]
    fn incrementally_typed_kitty_always_starts_a_visible_top_row_cat() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        let mut wd = WordDecorations::default();

        for (i, byte) in b"kitty".iter().copied().enumerate() {
            term.process(&[byte]);
            let now = t0 + Duration::from_millis(i as u64 * 40);
            wd.rescan(
                &term,
                g.rows as usize,
                g.cols as usize,
                &lex,
                &c,
                i as u64 + 1,
                now,
            );
            if i < 4 {
                assert!(
                    wd.occ.iter().all(|o| o.class != Class::Feline),
                    "a partial token must not trigger early"
                );
            }
        }

        let birth = t0 + Duration::from_millis(4 * 40);
        assert_eq!(
            wd.occ.iter().filter(|o| o.class == Class::Feline).count(),
            1,
            "the final y deterministically creates one kitty episode"
        );
        let (cats, _, _) = tick_cat_at(
            &mut wd,
            birth + Duration::from_millis(100),
            &c,
            g,
            None,
            true,
        );
        assert!(
            !cats.is_empty(),
            "a kitty typed on the top prompt row visibly peeks in"
        );
    }

    /// A downward peek is cut only at the text line: the art's bottom edge
    /// (`ay + h`) stays fixed through rise and descend, and only the top edge
    /// moves. No pixel buries the word it decorates (a viewport clamp that
    /// slid an Up sprite into view would cover it).
    #[test]
    fn a_downward_peek_is_cut_only_at_the_text_line() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        // Row 0 has no rows above, so the peek plan must resolve down.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[1;6Hkitty");
        let mut wd = scan_grid(&mut term, &c, g, t0, Some((0, 10)));
        assert!(
            feline(&wd).cat_peek_down,
            "a row-0 word has no habitable side above it — the plan must peek DOWN"
        );

        // Rise, dwell, descend; early frames may emit nothing (bake budget).
        let mut seen: Vec<(u16, u16, u16)> = Vec::new(); // (y, h, ay)
        for ms in (0..(CAT_RISE_MS + DWELL_QUIET_MS + CAT_DESCEND_MS + 400)).step_by(16) {
            let (free, _, _) =
                tick_cat_at(&mut wd, t0 + Duration::from_millis(ms), &c, g, None, true);
            for s in free.iter().filter(|s| matches!(s.z, FreeZ::UnderText)) {
                assert!(
                    s.y >= i32::from(g.cell_h) / 2,
                    "no sprite pixel may bury the top-row word it decorates: y={}",
                    s.y
                );
                seen.push((s.y as u16, s.h, s.ay));
            }
        }
        assert!(
            seen.len() >= 8,
            "the fixture must actually play a peek: {} frames",
            seen.len()
        );
        // Heights must genuinely vary, or the invariant below is vacuous.
        let hmin = seen.iter().map(|s| s.1).min().expect("non-empty");
        let hmax = seen.iter().map(|s| s.1).max().expect("non-empty");
        assert!(
            hmax > hmin,
            "the reveal must actually grow and shrink (h stuck at {hmin})"
        );

        let bottoms: Vec<u16> = seen.iter().map(|&(_, h, ay)| ay + h).collect();
        let b0 = bottoms[0];
        assert!(
            bottoms.iter().all(|&b| b == b0),
            "a DOWN peek must keep the art's BOTTOM edge on screen at every \
             frame — the cut belongs on the text line, never in open space. \
             Source bottoms seen: {bottoms:?}"
        );
    }

    /// The companion arrives a frame after the tick that scanned its word, by
    /// which time the caret is past the span, so the claim never latches;
    /// [`WordDecorations::caret_word`] still names the word and no twin pops.
    #[test]
    fn the_summoning_word_is_claimed_even_after_the_caret_moved_on() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        // `kitty` at row 3, cols 5..=9, caret one past its last: the only scan
        // that can name the summoning word.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[4;6Hkitty");
        let on_span = (3u16, 10u16);
        // One keystroke later: the first caret any companion frame sees.
        let past_span = (3u16, 11u16);
        assert!(
            caret_on_span(on_span, 3, 5, 9) && !caret_on_span(past_span, 3, 5, 9),
            "the rescan caret is on the word's span and the companion's is not"
        );
        let mut wd = scan_grid(&mut term, &c, g, t0, Some(on_span));
        let mut ctl = scan_grid(&mut term, &c, g, t0, Some(on_span));
        let occ = feline(&wd);
        assert_eq!((occ.row, occ.start_col, occ.end_col), (3, 5, 9));
        // No warm-up: the companion's first frame already has the caret off.
        let (mut held, mut control) = (Vec::new(), Vec::new());
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            held = companion_frame(&mut wd, now, &c, g, Some(past_span));
            control = companion_frame(&mut ctl, now, &c, g, None);
        }
        // The control proves the word is still mid-peek: the silence is the
        // claim.
        assert_eq!(
            ambient(&control).len(),
            1,
            "the word is still mid-peek at the sample instants"
        );
        assert!(
            ambient(&held).is_empty(),
            "the caret left the span before the companion existed — the claim \
             must still reach the word that summoned it"
        );
        assert_eq!(held.len(), 1, "exactly one cat on glass");
    }

    /// Suppression is scoped to the word under the companion: a feline word
    /// elsewhere peeks byte-identically to a frame with no companion, and so
    /// it does beside the resident pet standing rows away: yielding is a
    /// collision rule, not a mode.
    #[test]
    fn a_feline_word_away_from_the_caret_still_peeks_beside_the_companion() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        // Only the away word this time; the caret sits on a bare prompt row.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[6;1Hkitty\x1b[4;1H");
        let caret = (3u16, 0u16);
        // The pet stands in the top-right corner — feet on row 1's baseline,
        // flank against the right edge — nowhere near the word's rise.
        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(g.cell_h)).round() as i32;
        let nat_w = (nat_h as f32 * crate::kitty_pet::ART_ASPECT).round() as i32;
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let feet = 2 * i32::from(g.cell_h);
        let pet =
            CompanionOnGlass::at_body(Some(caret), (grid_w - nat_w, grid_w, feet - nat_h, feet));
        let mut wd = scan_grid(&mut term, &c, g, t0, Some(caret));
        let mut ctl = scan_grid(&mut term, &c, g, t0, Some(caret));
        let mut resident = scan_grid(&mut term, &c, g, t0, Some(caret));
        let (mut with_companion, mut control, mut with_pet) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            with_companion = companion_frame(&mut wd, now, &c, g, Some(caret));
            control = companion_frame(&mut ctl, now, &c, g, None);
            with_pet = tick_cat_companion(&mut resident, now, &c, g, Some(pet), true).0;
        }
        assert_eq!(ambient(&control).len(), 1, "the away kitty peeks");
        assert_eq!(
            ambient(&with_pet),
            ambient(&control),
            "a pet standing rows away leaves the word's peek untouched"
        );
        assert_eq!(
            ambient(&with_companion),
            ambient(&control),
            "a companion at the caret leaves every OTHER feline word alone"
        );
        assert_eq!(
            with_companion.len(),
            2,
            "one away cat plus the companion — the intended pair"
        );
    }

    /// Settled animal words each peek their own species head, in a pool
    /// separate from cats, with no Kitty Log rows.
    #[test]
    fn animal_words_peek_their_species_heads() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        // Two animal words on well-separated rows, caret on a bare prompt row.
        term.process(b"\x1b[3;1Hmonkey\x1b[7;1Hcamel\x1b[10;1H");
        let mut wd = scan_grid(&mut term, &c, g, t0, Some((9, 0)));
        // The rescan resolved each occurrence's species from its lexicon tag.
        let species: Vec<_> = wd.occ.iter().filter_map(|o| o.species).collect();
        assert_eq!(
            species,
            vec![AnimalGlyphId::Monkey, AnimalGlyphId::Camel],
            "each word carries its own authored head"
        );
        assert!(
            wd.occ.iter().all(|o| o.class == Class::Animal),
            "both words classify as animals"
        );
        // Into the dwell: both heads on glass once the bake budget settles.
        let mut free = Vec::new();
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            let (f, _, _) = tick_cat_at(&mut wd, now, &c, g, None, true);
            free = f;
        }
        let ambient: Vec<_> = free
            .iter()
            .filter(|s| matches!(s.z, FreeZ::UnderText))
            .collect();
        assert_eq!(ambient.len(), 2, "both species heads peek: {ambient:?}");
        // Animals never join the feline collectible surface.
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            0,
            "no Kitty Log sighting for a species head"
        );
    }

    /// While the resident pet stays up, the claim follows the caret to each new
    /// feline word, not just the session's first.
    #[test]
    fn a_second_feline_word_typed_while_the_companion_stays_up_is_claimed_too() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
        let mut term = Terminal::new(g.rows, g.cols);
        // Word A: `kitty` echoed at row 3, caret parked one past its last.
        term.process(b"\x1b[4;6Hkitty");
        let caret_a = (3u16, 10u16);
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            caret_a,
            "the caret parks one past word A"
        );
        let mut wd = WordDecorations::default();
        let mut ctl = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        ctl.rescan(&term, rows, cols, &lex, &c, 1, t0);
        let a_ident = feline(&wd).ident;
        // The pet stays up from here on; with the caret on A the claim latches.
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(100 + 16 * k);
            tick_cat_at(&mut wd, now, &c, g, Some(caret_a), true);
            tick_cat_at(&mut ctl, now, &c, g, None, true);
        }
        assert_eq!(
            wd.companion_claim,
            Some(a_ident),
            "the first word is claimed while the caret sits on it"
        );
        // Burn A's one-shot out (worst case 4580 ms), so later sprites are B's.
        let a_spent = t0 + Duration::from_millis(5000);
        tick_cat_at(&mut wd, a_spent, &c, g, Some(caret_a), true);
        tick_cat_at(&mut ctl, a_spent, &c, g, None, true);
        assert!(
            wd.persist[&a_ident].peek_done,
            "word A's one-shot has burned out"
        );
        // Word B on the bottom row, columns clear of A, while the pet stays up.
        term.process(b"\x1b[6;13Hkitty");
        let caret_b = (5u16, 17u16);
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            caret_b,
            "the caret parks one past word B"
        );
        let b_born = a_spent + Duration::from_millis(16);
        wd.rescan(&term, rows, cols, &lex, &c, 2, b_born);
        ctl.rescan(&term, rows, cols, &lex, &c, 2, b_born);
        let b_ident = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Feline && o.row == 5)
            .expect("the typed word B is on screen")
            .ident;
        // A few frames with the caret parked one past B, the pet still up.
        for k in 0..3u64 {
            let now = b_born + Duration::from_millis(20 + 16 * k);
            tick_cat_at(&mut wd, now, &c, g, Some(caret_b), true);
            tick_cat_at(&mut ctl, now, &c, g, None, true);
        }
        assert_eq!(
            wd.companion_claim,
            Some(b_ident),
            "the claim FOLLOWED the caret to word B — pet-blind latching kept it on A"
        );
        // The caret leaves both spans for B's whole peek; without the
        // re-target the claim would still name A and B would pop a twin.
        let away = (1u16, 0u16);
        let mut control_peeked = false;
        for k in 0..46u64 {
            let now = b_born + Duration::from_millis(100 + 100 * k);
            let (held, _, _) = tick_cat_at(&mut wd, now, &c, g, Some(away), true);
            let (loose, _, _) = tick_cat_at(&mut ctl, now, &c, g, None, true);
            assert!(
                held.iter().all(|s| !matches!(s.z, FreeZ::UnderText)),
                "sample {k}: word B peeked an ambient cat out from under the resident companion"
            );
            control_peeked |= loose.iter().any(|s| matches!(s.z, FreeZ::UnderText));
        }
        assert!(
            control_peeked,
            "non-vacuity: with no companion up, word B peeks somewhere in this window"
        );
    }

    /// The yield box is the pet's live body, not a caret band: a word under
    /// it stays down.
    #[test]
    fn a_word_under_the_pet_body_never_peeks() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        // The caret sits rows away, off every span, isolating the pixel yield.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[6;1Hkitty\x1b[4;1H");
        let caret = (3u16, 0u16);
        // Where the ambient cat lands, from the control twin (settled frames).
        let mut ctl = scan_grid(&mut term, &c, g, t0, Some(caret));
        let mut control = Vec::new();
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            control = tick_cat_companion(&mut ctl, now, &c, g, None, true).0;
        }
        let cats = ambient(&control);
        assert_eq!(cats.len(), 1, "the word peeks with nothing on glass");
        let cat = cats[0];
        // A pet-shaped body (ART_ROWS × ART_ASPECT) standing on the peeking
        // head, rows away from the caret.
        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(g.cell_h)).round() as i32;
        let nat_w = (nat_h as f32 * crate::kitty_pet::ART_ASPECT).round() as i32;
        let feet = cat.y + i32::from(cat.h);
        let x0 = cat.x + i32::from(cat.w) / 2 - nat_w / 2;
        // No caret cell (DECTCEM hidden): the live body alone must hold custody.
        let companion = CompanionOnGlass::at_body(None, (x0, x0 + nat_w, feet - nat_h, feet));
        let mut wd = scan_grid(&mut term, &c, g, t0, Some(caret));
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            let (free, _, _) = tick_cat_companion(&mut wd, now, &c, g, Some(companion), true);
            assert!(
                ambient(&free).is_empty(),
                "frame {k}: the word peeked its cat straight under the pet's body"
            );
        }
    }

    /// `kitty`, its cat done, `clear`, retype at the same column, right away
    /// or after the spent episode grace-expired: the retype keys identically
    /// (ident folds no row; blank clears never taint), and the typed witness
    /// makes it a fresh second cat. After [`GRACE_TTL`] the departure's
    /// session-lived done mark would otherwise make it born-done (the re-arm
    /// cannot require a still-resident old episode).
    #[test]
    fn a_retype_after_clear_plays_a_second_cat_even_after_expiry() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        for (label, wait) in [
            ("right after the clear", Duration::ZERO),
            ("after grace expiry", GRACE_TTL + Duration::from_secs(1)),
        ] {
            let t0 = Instant::now();
            let mut term = Terminal::new(g.rows, g.cols);
            let mut wd = WordDecorations::default();
            term.process(b"kitty"); // caret parks at end_col + 1 — the typed witness
            wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
            let first = feline(&wd).clone();
            let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(100), &c, g);
            assert!(
                !cats.is_empty(),
                "{label}: PRECONDITION — the first kitty plays"
            );
            // Run the one-shot to completion (worst-case peek is 4580 ms).
            let done_t = t0 + Duration::from_millis(5000);
            tick_cat(&mut wd, done_t, &c, g);
            assert!(
                wd.persist[&first.ident].peek_done,
                "{label}: PRECONDITION — the first one-shot completes"
            );
            let done_key = first.ident ^ wd.persist[&first.ident].ctx_fp;

            // `clear`: one blank frame (grace, never taint), prompt returns home.
            term.process(b"\x1b[2J\x1b[H");
            let cleared = done_t + Duration::from_millis(16);
            wd.rescan(&term, rows, cols, &lex, &c, 2, cleared);
            assert!(wd.occ.iter().all(|o| o.class != Class::Feline));
            let mut seq = 3;
            if !wait.is_zero() {
                wd.rescan(&term, rows, cols, &lex, &c, seq, cleared + wait);
                seq += 1;
                assert!(
                    !wd.persist.contains_key(&first.ident),
                    "{label}: PRECONDITION — the episode really has grace-expired"
                );
                assert!(
                    wd.done_marks.contains_key(&done_key),
                    "{label}: PRECONDITION — …while its session done mark survives, \
                     which is the asymmetry the bug lived in"
                );
            }

            // Retype at the exact same row and column, with the key that typed
            // it: the caret lands after the `y` either way, so the keystroke is
            // what makes this a retype rather than a redraw.
            term.process(b"kitty");
            let retyped_at = cleared + wait + Duration::from_millis(16);
            wd.note_typed_edit(retyped_at, None);
            wd.rescan(&term, rows, cols, &lex, &c, seq, retyped_at);
            let second = feline(&wd).clone();
            assert_eq!(
                second.ident, first.ident,
                "{label}: PRECONDITION — the retype re-keys onto the same ledger row"
            );
            assert_eq!(
                second.appeared, retyped_at,
                "{label}: the retyped kitty is a FRESH episode, not an adoption"
            );
            assert!(!second.inert, "{label}: the retype must not be born-done");
            let (cats, _, _) = tick_cat(&mut wd, retyped_at + Duration::from_millis(100), &c, g);
            assert!(
                !cats.is_empty(),
                "{label}: a genuinely retyped kitty plays a SECOND cat"
            );
        }
    }

    // The typed-witness scope fixture shared by the place, pane and tab tests
    // below: `kitty` on row 2 above an ordinary prompt; a gap frame that
    // overwrites row 2 nonblank (what taints the episode, so its return argues
    // on the typed witness rather than on grace); two caret parks that change
    // no cell; and a restore that puts the word back with the caret one past
    // the `y`, which is what both a retype and a TUI redraw look like.
    const WITNESS_BASE: &[u8] = b"\r\n\r\nkitty\r\n\r\n$ ok";
    const WITNESS_GAP: &[u8] = b"\x1b[3;1H------";
    /// Row 2, col 6 — beside `kitty`.
    const PARK_ON_WORD: &[u8] = b"\x1b[3;7H";
    /// Row 5, col 10 — the prompt.
    const PARK_FAR_AWAY: &[u8] = b"\x1b[6;11H";
    const WITNESS_RESTORE: &[u8] = b"\x1b[3;1Hkitty \x1b[3;6H";

    /// Drive `term`'s `kitty` through its whole one-shot (stamped in place),
    /// then the tainting gap frame with the caret at `park`. Returns its ident.
    fn play_out_and_taint(
        wd: &mut WordDecorations,
        term: &mut Terminal,
        park: &[u8],
        t0: Instant,
        label: &str,
    ) -> u64 {
        let (lex, c, g) = (lex(), cfg(), geom20());
        let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        term.process(WITNESS_BASE);
        wd.rescan(term, rows, cols, &lex, &c, 1, t0);
        let first = feline(wd).clone();
        assert!(!first.inert, "{label}: PRECONDITION — the first cat arms");
        let (cats, _, _) = tick_cat(wd, at(100), &c, g);
        assert!(!cats.is_empty(), "{label}: PRECONDITION — it draws");
        tick_cat(wd, at(5000), &c, g);
        let ep = wd.persist[&first.ident];
        assert!(
            ep.peek_done,
            "{label}: PRECONDITION — the one-shot completes"
        );
        assert!(
            wd.done_marks.contains_key(&(first.ident ^ ep.ctx_fp)),
            "{label}: PRECONDITION — the guard is stamped in place"
        );
        term.process(WITNESS_GAP);
        term.process(park);
        wd.rescan(term, rows, cols, &lex, &c, 2, at(5016));
        assert!(
            wd.persist[&first.ident].continuity_tainted,
            "{label}: PRECONDITION — the overwrite taints, so every arm argues \
             about the same branch"
        );
        first.ident
    }

    /// Repaint the word back at `back_at` and report `(inert, drew no cat)`.
    fn restore_verdict(
        wd: &mut WordDecorations,
        term: &mut Terminal,
        ident: u64,
        back_at: Instant,
        label: &str,
    ) -> (bool, bool) {
        let (lex, c, g) = (lex(), cfg(), geom20());
        term.process(WITNESS_RESTORE);
        wd.rescan(
            term,
            usize::from(g.rows),
            usize::from(g.cols),
            &lex,
            &c,
            3,
            back_at,
        );
        let back = feline(wd).clone();
        assert_eq!(
            back.ident, ident,
            "{label}: PRECONDITION — the restore re-keys identically"
        );
        assert!(
            wd.last_caret.is_some_and(|cell| caret_on_span(
                cell,
                back.row,
                back.start_col,
                back.end_col
            )),
            "{label}: PRECONDITION — the restored caret is parked on the word, \
             so only the keystroke can separate the verdicts"
        );
        let (cats, _, _) = tick_cat(wd, back_at + Duration::from_millis(100), &c, g);
        (back.inert, cats.is_empty())
    }

    /// A key committed five rows away does not license a repaint that drops a
    /// spent `kitty` under the caret; the same key on the word does.
    #[test]
    fn an_unrelated_edit_does_not_license_a_repaint_replay() {
        let g = geom20();
        let t0 = Instant::now();
        let mut verdicts = Vec::new();
        for (label, park) in [("retype", PARK_ON_WORD), ("unrelated edit", PARK_FAR_AWAY)] {
            let mut term = Terminal::new(g.rows, g.cols);
            let mut wd = WordDecorations::default();
            let ident = play_out_and_taint(&mut wd, &mut term, park, t0, label);
            // The keystroke, identical in both arms but for the caret it was
            // typed at (captured by the gap rescan).
            let key_at = t0 + Duration::from_millis(5036);
            let back_at = key_at + Duration::from_millis(12);
            wd.note_typed_edit(key_at, None);
            assert!(
                wd.typed_edit_witness(back_at),
                "{label}: PRECONDITION — the time/pane half of the witness is \
                 live in BOTH arms, so only the PLACE can decide"
            );
            verdicts.push((
                label,
                restore_verdict(&mut wd, &mut term, ident, back_at, label),
            ));
        }

        assert_eq!(
            verdicts[0],
            ("retype", (false, false)),
            "CONVERSE: a key committed ON the word is a retype and must still \
             play its cat"
        );
        assert_eq!(
            verdicts[1],
            ("unrelated edit", (true, true)),
            "a key committed five rows away in another widget must not license \
             a redraw's replay — that is the owner's 'new kitties appear when \
             the screen draws'"
        );
    }

    /// The typed witness takes its caret from the pane the key went to, not the
    /// bound one: both panes hold identical text and the arms differ only in
    /// which pane's caret is on the word.
    #[test]
    fn a_split_keystroke_is_attributed_to_the_pane_it_was_typed_in() {
        let g = geom20();
        let t0 = Instant::now();
        const PANE_A: u64 = 1;
        const PANE_B: u64 = 2;

        let mut verdicts = Vec::new();
        for (label, park_a, park_b) in [
            ("typed ON the word in B", PARK_FAR_AWAY, PARK_ON_WORD),
            ("typed FAR from the word in B", PARK_ON_WORD, PARK_FAR_AWAY),
        ] {
            let mut wd = WordDecorations::default();
            let mut term_a = Terminal::new(g.rows, g.cols);
            let mut term_b = Terminal::new(g.rows, g.cols);
            // Two panes side by side, disjoint pixel origins.
            wd.bind_pane(PANE_A, (0, 0));
            play_out_and_taint(&mut wd, &mut term_a, park_a, t0, label);
            wd.bind_pane(PANE_B, (400, 0));
            let ident_b = play_out_and_taint(&mut wd, &mut term_b, park_b, t0, label);

            // Pane A is left bound while the user types in B.
            wd.bind_pane(PANE_A, (0, 0));
            let key_at = t0 + Duration::from_millis(5036);
            let back_at = key_at + Duration::from_millis(12);
            wd.note_typed_edit(key_at, Some(PANE_B));
            assert_eq!(
                wd.typed_edit.expect("the key was recorded").cell,
                Some(caret_of(&term_b)),
                "{label}: the recorded caret must be PANE B's — the pane the \
                 keystroke went to — not the bound pane's"
            );
            assert!(
                !wd.typed_edit_witness(back_at),
                "{label}: PRECONDITION — while A is bound, B's keystroke is no \
                 witness for A's scan"
            );

            // Pane B repaints its word back, exactly as a TUI redraw would.
            wd.bind_pane(PANE_B, (400, 0));
            assert!(
                wd.typed_edit_witness(back_at),
                "{label}: PRECONDITION — the time/pane half IS live for B in \
                 both arms, so only the recorded PLACE can decide"
            );
            verdicts.push((
                label,
                restore_verdict(&mut wd, &mut term_b, ident_b, back_at, label),
            ));
        }

        assert_eq!(
            verdicts[0],
            ("typed ON the word in B", (false, false)),
            "a retype committed IN PANE B, on B's own word, must re-arm and \
             draw B's cat — reading pane A's caret refuses it"
        );
        assert_eq!(
            verdicts[1],
            ("typed FAR from the word in B", (true, true)),
            "and a key committed five rows away IN PANE B must not license B's \
             redraw replay just because pane A's caret happens to sit on pane \
             A's copy of the word"
        );
    }

    /// The active-grid caret of a fixture terminal, in the (row, col) shape
    /// [`WordDecorations::last_caret`] records.
    fn caret_of(term: &Terminal) -> (u16, u16) {
        let cp = term.cursor();
        (cp.row, cp.col)
    }

    /// After a split collapses, the survivor's keys are witnessed from the live
    /// caret (its parked copy is frozen), and the unbound wildcard admits them.
    #[test]
    fn a_collapsed_split_still_witnesses_the_survivors_keystrokes() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();

        // The survivor (pane 1) is scanned and parked: its parked caret freezes.
        let mut survivor = Terminal::new(g.rows, g.cols);
        survivor.process(b"kitty");
        wd.bind_pane(1, (0, 0));
        wd.rescan(&survivor, rows, cols, &lex, &c, 1, t0);
        let parked_caret = wd.last_caret.expect("the survivor's caret was scanned");
        wd.bind_pane(2, (400, 0));

        // Pane 2 closes. The engine unbinds; pane 1's state is still parked.
        wd.retain_panes(|k| k == 1);
        assert_eq!(wd.bound, None, "PRECONDITION: the live pane departed");
        assert!(wd.parked.contains_key(&1), "PRECONDITION: pane 1 is parked");

        // The single-pane renderer takes over; the caret has moved since.
        survivor.process(b"\r\n\r\nkitty");
        wd.rescan(
            &survivor,
            rows,
            cols,
            &lex,
            &c,
            2,
            t0 + Duration::from_millis(16),
        );
        let live_caret = wd.last_caret.expect("the single-pane rescan re-stamped it");
        assert_ne!(
            live_caret, parked_caret,
            "PRECONDITION: the parked copy really is stale, so the two answers \
             below are distinguishable"
        );

        let key_at = t0 + Duration::from_millis(32);
        wd.note_typed_edit(key_at, Some(1));
        assert_eq!(
            wd.typed_edit.expect("recorded").cell,
            Some(live_caret),
            "the survivor's caret is the LIVE one — its parked slot froze when \
             the split was still up"
        );
        assert!(
            wd.typed_edit_witness(key_at),
            "and an unbound single-grid scan must still witness the key, or a \
             window that was once split never re-arms again"
        );
        // Also once the renderer declares the survivor's session.
        wd.set_scan_session(Some(1));
        assert!(
            wd.typed_edit_witness(key_at),
            "declaring the survivor's own session must keep witnessing it"
        );
    }

    /// A declared session retires a stale pane binding (nothing unbinds when
    /// the user moves from a split tab to an unsplit one), or every key in the
    /// unsplit tab would be refused. Both directions are asserted.
    #[test]
    fn declaring_an_unsplit_session_retires_a_stale_pane_binding() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();

        /// A pane of the split tab the user is leaving.
        const PANE: u64 = 1;
        /// The session of the unsplit tab they arrive at.
        const TAB: u64 = 2;

        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"kitty");
        wd.bind_pane(PANE, (400, 0));
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

        let key_at = t0 + Duration::from_millis(16);
        wd.note_typed_edit(key_at, Some(TAB));
        assert!(
            !wd.typed_edit_witness(key_at),
            "PRECONDITION: while the split tab's pane is bound, the OTHER tab's \
             key is correctly no witness for it"
        );

        // The user switches to the unsplit tab; its renderer declares itself.
        wd.set_scan_session(Some(TAB));
        assert_eq!(
            wd.bound, None,
            "the stale binding is retired, not merely outranked"
        );
        assert!(
            wd.parked.contains_key(&PANE),
            "and the split tab's pane keeps its episodes, parked — going back \
             must not find its cats wiped"
        );
        assert_eq!(
            wd.pane_px,
            (0, 0),
            "an unbound engine's grid origin is the window's own"
        );
        assert!(
            wd.needs_rescan(1),
            "the live fields describe a grid that is no longer on glass, so the \
             declared session owes the engine a fresh scan"
        );
        assert!(
            wd.typed_edit_witness(key_at),
            "and the unsplit tab's own keystroke is a witness for its own scan \
             — the stale binding was refusing every one of them"
        );
    }

    /// On an unsplit window, a key committed in tab A does not license a
    /// repaint in tab B to re-arm a spent cat. The arms differ only in the
    /// key's tab; the first proves the fixture re-arms.
    #[test]
    fn an_edit_in_one_tab_does_not_license_another_tabs_replay_when_unsplit() {
        let g = geom20();
        let t0 = Instant::now();
        /// The tab that is on glass for the whole fixture.
        const TAB_A: u64 = 11;
        /// A tab that is not on glass — only ever named by a keystroke.
        const TAB_B: u64 = 12;

        let mut verdicts = Vec::new();
        for (label, key_tab) in [("same tab", TAB_A), ("another tab", TAB_B)] {
            let mut wd = WordDecorations::default();
            let mut term = Terminal::new(g.rows, g.cols);
            // The unsplit host binds nothing and declares the tab it shows.
            wd.set_scan_session(Some(TAB_A));
            let ident = play_out_and_taint(&mut wd, &mut term, PARK_ON_WORD, t0, label);

            // The edit key, in the tab on glass or the other one.
            let key_at = t0 + Duration::from_millis(5036);
            let back_at = key_at + Duration::from_millis(12);
            wd.note_typed_edit(key_at, Some(key_tab));
            assert_eq!(
                wd.typed_edit_witness(back_at),
                key_tab == TAB_A,
                "{label}: PRECONDITION — a key witnesses the scan of its OWN \
                 tab and no other; this is the discriminator the verdicts below \
                 must be reading"
            );
            verdicts.push((
                label,
                restore_verdict(&mut wd, &mut term, ident, back_at, label),
            ));
        }

        assert_eq!(
            verdicts[0],
            ("same tab", (false, false)),
            "a retype in the tab that is on glass must still re-arm and draw — \
             this is a scope, not a deletion"
        );
        assert_eq!(
            verdicts[1],
            ("another tab", (true, true)),
            "but a key committed in ANOTHER tab must not license this one's \
             repaint replay: once per kitty word, per tab"
        );
    }

    /// `kitty` at the same cell in two tabs shares an ident, yet each tab plays
    /// its own cat: [`WordDecorations::set_scan_session`] parks each tab's map.
    /// A control arm and a return arm rule out "every rescan plays" and
    /// "tab A's state was wiped".
    #[test]
    fn identical_words_in_two_tabs_each_get_their_own_cat() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        const TAB_A: u64 = 31;
        const TAB_B: u64 = 32;

        // Two terminals with identical text at identical cells.
        let text = b"\r\n\r\nkitty\r\n\r\n$ ok".as_slice();
        let mut tab_a = Terminal::new(g.rows, g.cols);
        tab_a.process(text);
        let mut tab_b = Terminal::new(g.rows, g.cols);
        tab_b.process(text);

        let mut wd = WordDecorations::default();

        // ── tab A: the word appears and plays its one cat, to completion.
        wd.set_scan_session(Some(TAB_A));
        wd.rescan(&tab_a, rows, cols, &lex, &c, 1, t0);
        let a_ident = feline(&wd).ident;
        let (cats, _, _) = tick_cat(&mut wd, at(100), &c, g);
        assert!(!cats.is_empty(), "tab A's kitty plays its cat");
        tick_cat(&mut wd, at(5000), &c, g);
        assert!(
            wd.persist[&a_ident].peek_done,
            "PRECONDITION: and the one-shot is spent, which is what makes the \
             two arms below distinguishable"
        );

        // ── control: tab A rescanned and ticked as tab B will be.
        wd.rescan(&tab_a, rows, cols, &lex, &c, 2, at(5016));
        let (cats, _, _) = tick_cat(&mut wd, at(5116), &c, g);
        assert!(
            cats.is_empty(),
            "one tab, one cat: a plain repaint of the same word must stay silent"
        );

        // ── tab B: the user switches. Same word, same cell, different session.
        wd.set_scan_session(Some(TAB_B));
        wd.rescan(&tab_b, rows, cols, &lex, &c, 1, at(5116));
        let b_ident = feline(&wd).ident;
        assert_eq!(
            b_ident, a_ident,
            "PRECONDITION: the two words are INDISTINGUISHABLE by identity — \
             the scope this test pins is the episode map, not the key"
        );
        let (cats, _, _) = tick_cat(&mut wd, at(5216), &c, g);
        assert!(
            !cats.is_empty(),
            "tab B's kitty is tab B's own first sighting and owes its own cat — \
             a shared map hands it tab A's spent episode and it never appears"
        );

        // ── back: tab A's state was parked; its word is still spent.
        wd.set_scan_session(Some(TAB_A));
        wd.rescan(&tab_a, rows, cols, &lex, &c, 3, at(5232));
        let (cats, _, _) = tick_cat(&mut wd, at(5332), &c, g);
        assert!(
            cats.is_empty(),
            "returning to tab A finds its own spent episode where it left it"
        );
    }

    /// An unscoped pending ignition is not session 0's: a prune under session
    /// 0 must not cancel the still-queued nova.
    #[test]
    fn an_unbound_scans_pending_ignition_is_not_session_zeros_to_cancel() {
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        assert_eq!(
            wd.scan_scope(),
            None,
            "PRECONDITION: a fresh engine names neither a pane nor a session"
        );
        for (owner, cx) in [(1u64, 100i32), (2, 900), (3, 1700)] {
            grant_pane_ignition(
                &mut wd.ignitions,
                None,
                wd.pane_px,
                owner,
                t0,
                (cx, 100),
                0.0,
            );
        }
        let pending = wd
            .ignitions
            .iter()
            .find(|r| r.start > t0)
            .map(|r| r.start)
            .expect("PRECONDITION: the third grant is deferred past the window");

        // A tab declares itself — and its id happens to be zero.
        wd.set_scan_session(Some(0));
        wd.prune_ignitions(t0 + Duration::from_millis(10));
        assert!(
            wd.ignitions
                .iter()
                .any(|r| r.pane.is_none() && r.start == pending),
            "session 0 must not be able to cancel the unbound scan's queued \
             nova: `None` is a key no session can hold"
        );
    }

    /// A `kitty` typed while 8 scrolled-off cats are mid-peek still draws: the
    /// slot census counts visible occurrences only.
    #[test]
    fn typed_kitty_after_dense_cat_output_is_not_slot_starved() {
        let lex = lex();
        let c = cfg();
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 6,
            cols: 24,
        };
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        let mut wd = WordDecorations::default();

        // 12 cat words over two rows: more than MAX_CATS can ever show.
        term.process(b"cat cat cat cat cat cat\r\ncat cat cat cat cat cat");
        wd.rescan(&term, g.rows as usize, g.cols as usize, &lex, &c, 1, t0);
        tick_cat(&mut wd, t0 + Duration::from_millis(16), &c, g);
        assert_eq!(
            wd.persist
                .values()
                .filter(|e| e.shown_as == Some(KittyShownAs::Cat))
                .count(),
            MAX_CATS,
            "the dense screen arms every slot"
        );

        // One second in: 12 grace-resident episodes, 8 cats still mid-peek.
        term.process(b"\x1b[2J\x1b[Hkitty");
        let t1 = t0 + Duration::from_millis(1000);
        wd.rescan(&term, g.rows as usize, g.cols as usize, &lex, &c, 2, t1);
        // Post-clear the only visible occurrence is the typed kitty.
        let typed = feline(&wd).clone();
        let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(100), &c, g);
        assert_eq!(
            wd.persist[&typed.ident].shown_as,
            Some(KittyShownAs::Cat),
            "scrolled-off history must not starve the typed word's slot"
        );
        assert!(
            !cats.is_empty(),
            "the typed kitty draws even right after cat-word-dense output"
        );

        // By +6 s every armed peek is done; the scrolled-off ones only via the
        // prepass sweep.
        tick_cat(&mut wd, t1 + Duration::from_millis(6000), &c, g);
        assert!(
            wd.persist
                .values()
                .all(|e| e.shown_as != Some(KittyShownAs::Cat) || e.peek_done),
            "scrolled-off mid-peek episodes complete by wall clock"
        );
    }

    /// Output over a dwelling cat hides it and pauses its clock; it resumes
    /// where it left off instead of burning to Done while invisible.
    #[test]
    fn occlusion_mid_dwell_pauses_the_peek_and_resumes_when_rows_clear() {
        let (rows, cols) = (6usize, 20usize);
        let lex = lex();
        let c = cfg();
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: rows as u16,
            cols: cols as u16,
        };
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\r\n\r\n\r\nkitty"); // word on row 3; rows 1-2 blank above
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let bg = rgb3_to_u32(snap.cells[3][0].bg);
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            1,
            t0,
            g,
            bg,
            None,
        );
        let ident = feline(&wd).ident;
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
        assert!(!cats.is_empty(), "the kitty is peeking mid-dwell");

        // Output floods the footprint rows mid-dwell (trimmed snapshot rows are
        // extended from a blank cell first).
        let mut template = snap.cells[3][0];
        template.ch = ' ';
        template.wide = false;
        // Wall both bands: with only the upper one walled the cat would peek
        // down rather than pause.
        for r in [1usize, 2, 4, 5] {
            snap.cells[r].resize(cols, template);
            for cell in &mut snap.cells[r] {
                cell.ch = 'X';
            }
        }
        let t1 = t0 + Duration::from_millis(1000);
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            2,
            t1,
            g,
            bg,
            None,
        );
        let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(16), &c, g);
        assert!(cats.is_empty(), "an occluded cat emits nothing");
        assert!(
            wd.persist[&ident].peek_pause.is_some(),
            "occlusion latches the clearance pause"
        );

        // Far past the nominal peek length: the paused clock must not burn out.
        let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(8000), &c, g);
        assert!(cats.is_empty());
        assert!(
            !wd.persist[&ident].peek_done,
            "a paused peek never silently completes while invisible"
        );

        // The rows clear again: the cat resumes mid-dwell and then finishes.
        for r in [1usize, 2, 4, 5] {
            for cell in &mut snap.cells[r] {
                cell.ch = ' ';
            }
        }
        let t3 = t1 + Duration::from_millis(9000);
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            3,
            t3,
            g,
            bg,
            None,
        );
        let (cats, _, _) = tick_cat(&mut wd, t3 + Duration::from_millis(16), &c, g);
        assert!(
            !cats.is_empty(),
            "the cat resumes its dwell when the rows clear"
        );
        assert!(!wd.persist[&ident].peek_done);
        let (cats, _, _) = tick_cat(&mut wd, t3 + Duration::from_millis(6000), &c, g);
        assert!(cats.is_empty(), "the resumed peek runs to completion");
        assert!(
            wd.persist[&ident].peek_done,
            "after resuming, the one-shot completes normally"
        );
    }

    /// Past [`MAX_OCCURRENCES`], whole top rows drop so the prompt row keeps its
    /// slots, in row-major order, stable across rescans.
    #[test]
    fn bottom_rows_keep_occurrence_slots_on_a_match_saturated_screen() {
        let (rows, cols) = (5usize, 160usize);
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        // Rows 0-3: 40 bare `cat` matches each (160 raw matches > 128).
        let dense = b"cat ".repeat(39);
        for _ in 0..4 {
            term.process(&dense);
            term.process(b"cat\r\n");
        }
        term.process(b"kitty"); // the prompt row (4), caret after the word
        let mut wd = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

        // Budget: the kitty row costs 2, rows 3..1 cost 40 each, row 0 does not
        // fit, so rows 1-4 keep 3·40 + 1 occurrences.
        assert_eq!(wd.occ.len(), 121, "kept suffix is rows 1..=4");
        assert!(
            wd.occ.iter().all(|o| o.row >= 1),
            "whole TOP rows are dropped, never bottom ones"
        );
        let prompt_kitty = wd
            .occ
            .iter()
            .find(|o| o.row == 4)
            .expect("the prompt row keeps its occurrence slot");
        assert_eq!(prompt_kitty.start_col, 0);
        assert_eq!(prompt_kitty.appeared, t0);
        assert!(
            wd.occ
                .windows(2)
                .all(|w| (w[0].row, w[0].start_col) < (w[1].row, w[1].start_col)),
            "row-major order is preserved within the kept rows"
        );
        assert!(
            wd.scan_row_occupied[0],
            "skipped dense rows still record truthful occupancy for the taint sweep"
        );

        // An identical rescan keeps the suffix and continues the prompt kitty.
        wd.rescan(
            &term,
            rows,
            cols,
            &lex,
            &c,
            2,
            t0 + Duration::from_millis(16),
        );
        assert_eq!(wd.occ.len(), 121);
        let again = wd
            .occ
            .iter()
            .find(|o| o.row == 4)
            .expect("prompt slot survives the second pass");
        assert_eq!(
            again.appeared, t0,
            "the same screen re-adopts the same episode (no truncation churn)"
        );
    }

    /// `fold_free` covers every field, z and sampler included, and the signed
    /// origin by bit pattern.
    #[test]
    fn fold_free_covers_every_field_including_z_and_sampler() {
        let s = FreeSprite {
            x: -3,
            y: 5,
            w: 10,
            h: 12,
            ax: 1,
            ay: 2,
            aw: 10,
            ah: 12,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
            z: FreeZ::UnderText,
            sampler: FreeSampler::Nearest,
        };
        let h0 = 0xcbf2_9ce4_8422_2325u64;
        let base = fold_free(h0, &s);
        assert_ne!(
            base,
            fold_free(
                h0,
                &FreeSprite {
                    z: FreeZ::OverText,
                    ..s
                }
            ),
            "a z flip must move the fingerprint"
        );
        assert_ne!(
            base,
            fold_free(
                h0,
                &FreeSprite {
                    sampler: FreeSampler::Linear,
                    ..s
                }
            ),
            "a sampler flip must move the fingerprint"
        );
        assert_ne!(
            base,
            fold_free(h0, &FreeSprite { x: 3, ..s }),
            "the signed origin folds by bit pattern"
        );
        assert_ne!(
            base,
            fold_free(h0, &FreeSprite { alpha: 254, ..s }),
            "alpha folds"
        );
    }

    /// The settled cat moves during the dwell and never changes size.
    #[test]
    fn a_settled_keyword_kitty_breathes_without_resizing() {
        let now = Instant::now();
        let g = geom20();
        let build = || WordDecorations {
            occ: vec![cat_occ(2, 2, 6, 0, now)],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        // Sample across the dwell, clear of the entrance and the bounce.
        let sample = |wd: &mut WordDecorations, c: &DecoConfig, ms: u64| {
            let (fr, _, _) = tick_cat(wd, now + Duration::from_millis(ms), c, g);
            fr.iter()
                .find(|s| matches!(s.z, FreeZ::UnderText))
                .map(|s| (s.x, s.y, s.w, s.h, s.aw, s.ah))
        };

        let c = cfg();
        let mut wd = build();
        for t in [100u64, 300, 440, 700] {
            tick_cat(&mut wd, now + Duration::from_millis(t), &c, g);
        }
        let frames: Vec<_> = [900u64, 1150, 1400, 1650, 1900, 2150]
            .into_iter()
            .filter_map(|ms| sample(&mut wd, &c, ms))
            .collect();
        assert!(
            frames.len() >= 5,
            "the fixture must sit in the dwell: {} frames",
            frames.len()
        );
        // 1. It moves.
        let origins: std::collections::BTreeSet<(i32, i32)> =
            frames.iter().map(|f| (f.0, f.1)).collect();
        assert!(
            origins.len() > 1,
            "a settled word-cat must be alive, not a sticker: {origins:?}"
        );
        // 2. …by translation only: size and source window never move.
        let sizes: std::collections::BTreeSet<(u16, u16, u16, u16)> =
            frames.iter().map(|f| (f.2, f.3, f.4, f.5)).collect();
        assert_eq!(
            sizes.len(),
            1,
            "the idle pose translates and never resizes: {sizes:?}"
        );
    }

    // ─────────────── v3 §1.2 in-dwell life battery (pure time) ───────────────

    /// The peek clock latches at birth: quads land on the first ticked frame
    /// and same-frame siblings rise together.
    #[test]
    fn fresh_episode_latches_phase_clock_at_birth() {
        let lex = lex();
        let c = cfg();
        let g = EffectGeom {
            rows: 8,
            ..geom20()
        };
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(8, 20);
        term.process(b"\r\n\r\nnice kitty\r\n\r\nsame kitten");
        wd.rescan(&term, 8, 20, &lex, &c, 1, t0);
        for o in wd.occ.iter().filter(|o| o.spec.graphic.is_some()) {
            assert_eq!(
                wd.persist[&o.ident].phase_start,
                Some(t0),
                "the phase clock latches at birth, before any tick"
            );
        }
        // The very first ticked frame is mid-rise — no prior latch tick.
        let (fr, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
        // A sprite's word row: its bottom pixel (pinned at `row_top + chin`).
        let rows_at = |fr: &[FreeSprite]| -> std::collections::BTreeSet<i32> {
            fr.iter()
                .map(|s| (s.y + i32::from(s.h) - 1).div_euclid(20))
                .collect()
        };
        assert!(
            !fr.is_empty() && rows_at(&fr).len() == 2,
            "BOTH cats rise together from the first ticked frame (no \
             stagger), got {fr:?}"
        );
    }

    /// Freeze/thaw mid-rise resumes at the same phase; nothing completes while
    /// frozen, and the one-shot still runs to Done once.
    #[test]
    fn freeze_thaw_mid_rise_resumes_at_same_phase() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Control: an unfrozen twin engine on the same clock.
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na calm kitty");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        let mut ctl = WordDecorations::default();
        ctl.rescan(&term, 4, 20, &lex, &c, 1, t0);
        // First presents at t0 (both phase clocks latched at birth = t0).
        tick_cat_at(&mut wd, t0, &c, g, None, true);
        tick_cat_at(&mut ctl, t0, &c, g, None, true);
        // Both rise to t = 200 ms (mid-rise).
        let (q_a, _, _) = tick_cat_at(&mut wd, at(200), &c, g, None, true);
        let (q_c, _, _) = tick_cat_at(&mut ctl, at(200), &c, g, None, true);
        assert_eq!(q_a, q_c, "twin engines agree pre-freeze");
        assert!(!q_a.is_empty(), "mid-rise quads present");
        // Frozen 5 s then thawed: its t = 5200 frame equals the control's 200.
        wd.freeze(at(200));
        let (q_frozen, _, fp_frozen) = {
            let mut out = Vec::new();
            let mut ink = Vec::new();
            let mut fr = Vec::new();
            let mut nv = Vec::new();
            let fp = wd.tick(
                at(1000),
                &c,
                g,
                None,
                None,
                true,
                &mut out,
                &mut ink,
                &mut fr,
                &mut nv,
            );
            (fr, out, fp)
        };
        assert!(
            q_frozen.is_empty() && fp_frozen == 0,
            "frozen ticks emit nothing"
        );
        wd.thaw(at(5200));
        let (q_resumed, _, _) = tick_cat_at(&mut wd, at(5200), &c, g, None, true);
        assert_eq!(
            q_resumed, q_a,
            "thaw resumes at the SAME phase (nothing completed invisibly)"
        );
        // The episode is not done-marked mid-rise: it still runs to Done.
        let o = wd.occ[0].clone();
        let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        let (q_mid, _, _) = tick_cat_at(&mut wd, at(5000 + CAT_RISE_MS + 300), &c, g, None, true);
        assert!(!q_mid.is_empty(), "the dwell continues after the thaw");
        let (q_done, _, _) = tick_cat_at(&mut wd, at(5000 + total + 50), &c, g, None, true);
        assert!(
            q_done.is_empty(),
            "the shifted one-shot completes exactly once"
        );
    }

    /// Tier-1 conformance (v3 §1.3) with the `OneShotPeek` model: phase steps,
    /// short occlusion, freeze/thaw mid-rise, and a born-done re-appearance.
    #[test]
    fn one_shot_peek_conformance_real_engine() {
        let m = aterm_models_effects::one_shot_peek_model();
        let mut st = m.init_state();
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(6, 20);
        // Row 2: the head pose has its two rows of headroom (rows 0–1).
        term.process(b"\r\n\r\na calm kitty");
        let blank = Terminal::new(6, 20);
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        let o = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Feline)
            .unwrap()
            .clone();
        let variant =
            special_variant_v4(o.genome.magic).unwrap_or_else(|| cat_variant_v4(o.genome.gkey));
        let expected_h = u32::from(cat_geometry(&o, g, variant).hart);
        let dwell = peek_dwell_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        // Rise begins at birth (the rescan above): model Start.
        tick_cat_at(&mut wd, t0, &c, g, None, true);
        assert!(m.fire("Start", &mut st));
        let (q, _, _) = tick_cat_at(&mut wd, at(100), &c, g, None, true);
        assert!(!q.is_empty(), "rising");
        // Freeze/thaw mid-rise: the phase does not advance (no model action).
        let frozen_q = q.clone();
        wd.freeze(at(200));
        wd.thaw(at(3200));
        let (q, _, _) = tick_cat_at(&mut wd, at(3100 + 200), &c, g, None, true);
        assert!(
            q.iter().map(|p| u32::from(p.h)).sum::<u32>()
                >= frozen_q.iter().map(|p| u32::from(p.h)).sum::<u32>(),
            "the thawed rise continues from where it froze"
        );
        // Everything below runs on the shifted clock (freeze added 3000 ms).
        let sh = |ms: u64| at(3000 + ms);
        // Dwell: model Step (Rise → Dwell).
        let (q_dwell, _, _) = tick_cat_at(&mut wd, sh(CAT_RISE_MS + 300), &c, g, None, true);
        assert!(
            q_dwell.iter().map(|p| u32::from(p.h)).sum::<u32>() >= expected_h,
            "dwelling at the full rest reveal"
        );
        assert!(m.fire("Step", &mut st));
        let (q_mid, _, _) = tick_cat_at(&mut wd, sh(CAT_RISE_MS + dwell / 2), &c, g, None, true);
        assert!(
            !q_mid.is_empty(),
            "the mid-dwell present shows the body sprite"
        );
        // Occlusion < GRACE_TTL mid-dwell: the episode persists; no re-Rise.
        wd.rescan(&blank, 6, 20, &lex, &c, 2, sh(CAT_RISE_MS + dwell / 2));
        wd.rescan(&term, 6, 20, &lex, &c, 3, sh(CAT_RISE_MS + dwell / 2 + 500));
        let back = sh(CAT_RISE_MS + dwell - 200);
        let (q, _, _) = tick_cat_at(&mut wd, back, &c, g, None, true);
        assert!(
            q.iter().map(|p| u32::from(p.h)).sum::<u32>() >= expected_h,
            "the re-hit continues the SAME dwell (no re-Rise)"
        );
        assert!(m.check_invariant("NoRepeek", &st));
        // Descend: model Step (Dwell → Descend) then Finish at Done.
        let (q, _, _) = tick_cat_at(
            &mut wd,
            sh(CAT_RISE_MS + dwell + anticipation_ms(o.genome.gkey) + CAT_DESCEND_MS / 2),
            &c,
            g,
            None,
            true,
        );
        assert!(
            !q.is_empty() && q.iter().map(|p| u32::from(p.h)).sum::<u32>() < expected_h,
            "descending (shrinking reveal)"
        );
        assert!(m.fire("Step", &mut st));
        let (q, _, _) = tick_cat_at(&mut wd, sh(total + 20), &c, g, None, true);
        assert!(q.is_empty(), "Done");
        assert!(m.fire("Finish", &mut st));
        // Post-expiry re-appearance is born-done; the model's Repeek is a no-op.
        wd.rescan(&blank, 6, 20, &lex, &c, 4, sh(total + 100));
        let expired = sh(total + 100 + 15_000);
        wd.rescan(&blank, 6, 20, &lex, &c, 5, expired);
        assert!(wd.persist.is_empty(), "grace expiry swept the episode");
        wd.rescan(
            &term,
            6,
            20,
            &lex,
            &c,
            6,
            expired + Duration::from_millis(100),
        );
        let (q, _, _) = tick_cat_at(
            &mut wd,
            expired + Duration::from_millis(200),
            &c,
            g,
            None,
            true,
        );
        assert!(q.is_empty(), "the re-appearance is born-done: no entrance");
        assert!(m.fire("Repeek", &mut st), "healthy Repeek is a no-op");
        assert!(m.check_invariant("NoRepeek", &st));
        assert!(m.check_invariant("PhaseBounded", &st));
        assert_eq!(st[&"phase"], 4, "the model (like the cat) ends Done");
        assert_eq!(st[&"rises"], 1, "exactly one entrance, ever");
    }

    /// §10: `feline.magic = false` pins the ordinary build (no magic bake).
    #[test]
    fn feline_magic_off_disables_rare_cats() {
        let now = Instant::now();
        let mut o = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
        o.genome.magic = 8; // Nebula window
        let mut wd = WordDecorations {
            occ: vec![o],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let mut c = cfg();
        c.feline_magic = false;
        let g = geom20();
        let (_, out, _) = tick_cat(&mut wd, now + Duration::from_millis(120), &c, g);
        assert!(
            !out.iter().any(|d| matches!(d.glyph, DecoGlyph::Star4)),
            "magic=false ⇒ no Nebula aura"
        );
    }

    // ───────────────── §5.2/§5.4 geometry battery ─────────────────

    /// Through the shipping occurrence → geometry → BakeKey → atlas path, a
    /// normal head and the tall Maneki keep their authored aspect, and age
    /// scales both dimensions together.
    #[test]
    fn shipping_cat_sprites_preserve_authored_aspect_and_uniform_age_scale() {
        let now = Instant::now();
        let render = |gkey: u64, magic: u64| {
            let mut occurrence = cat_occ(2, 2, 6, gkey, now);
            occurrence.genome.magic = magic;
            let mut wd = WordDecorations {
                occ: vec![occurrence],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            let (sprites, _, _) = tick_cat(
                &mut wd,
                now + Duration::from_millis(DWELL_QUIET_MS),
                &cfg(),
                geom20(),
            );
            assert_eq!(sprites.len(), 1, "shipping bake must land one sprite");
            sprites[0]
        };
        let assert_aspect = |sprite: FreeSprite, variant: CatGlyphId| {
            let expected = f32::from(GLYPHS[variant as usize].aspect_x1000) / 1000.0;
            let actual = f32::from(sprite.w) / f32::from(sprite.h);
            assert!(
                (actual - expected).abs() <= 0.04,
                "{variant:?}: shipping sprite {actual:.3} != authored {expected:.3}"
            );
            assert_eq!((sprite.aw, sprite.ah), (sprite.w, sprite.h));
        };

        let head_variant = cat_variant_v4(GKEY_ADULT_HEAD);
        let head = render(GKEY_ADULT_HEAD, (200 << 24) | 100);
        assert_aspect(head, head_variant);

        let maneki = render(GKEY_ADULT_HEAD, 0);
        assert_aspect(maneki, CatGlyphId::SpecManeki);
        assert!(
            maneki.w < maneki.h,
            "the tall authored Maneki must not be stretched into the old wide head box"
        );

        let kitten_key = genome::gray_encode(0) << 13;
        let elder_key = genome::gray_encode(3) << 13;
        let kitten = render(kitten_key, (200 << 24) | 100);
        let elder = render(elder_key, (200 << 24) | 100);
        assert_aspect(kitten, cat_variant_v4(kitten_key));
        assert_aspect(elder, cat_variant_v4(elder_key));
        assert!(
            elder.w > kitten.w && elder.h > kitten.h,
            "age must scale width and height together: kitten={kitten:?}, elder={elder:?}"
        );
    }

    // ────────── v3 §1.1 alignment / done-marks / reset-table battery ──────────

    /// v3 §1.1 log-tail rotation at constant count: the survivor keeps its
    /// episode, the new bottom twin plays, the departed one grace-expires.
    #[test]
    fn rotation_alignment_plays_new_twin_and_pins_survivor() {
        let model = aterm_models_effects::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        // Twins at rows 0 and 2 (same word, same column ⇒ same seed).
        let mut twins = Terminal::new(6, 64);
        twins.process(b"dear kitty friend\r\n\r\ndear kitty friend");
        wd.rescan(&twins, 6, 64, &lex, &c, 1, t0);
        let cats: Vec<Occurrence> = wd
            .occ
            .iter()
            .filter(|o| o.class == Class::Feline)
            .cloned()
            .collect();
        assert_eq!(cats.len(), 2);
        let seed = cats[0].seed;
        let (id0, id1) = (mix(seed), mix(seed ^ ORDINAL_MIX));
        assert_eq!((cats[0].ident, cats[1].ident), (id0, id1));
        wd.persist.get_mut(&id0).expect("top twin").nova_done = true;
        wd.persist.get_mut(&id1).expect("surviving twin").nova_done = true;
        // The top twin scrolls off, a new twin enters at row 4.
        let mut rotated = Terminal::new(6, 64);
        rotated.process(b"\r\n\r\ndear kitty friend\r\n\r\ndear kitty friend");
        let t1 = t0 + Duration::from_secs(1);
        let births_before = wd.birth_seq;
        wd.rescan(&rotated, 6, 64, &lex, &c, 2, t1);
        let mut state = model.init_state();
        assert!(model.fire("RotatePair", &mut state));
        let cats2: Vec<Occurrence> = wd
            .occ
            .iter()
            .filter(|o| o.class == Class::Feline)
            .cloned()
            .collect();
        assert_eq!(cats2.len(), 2);
        // The row-2 survivor (ordinal 1 → 0) keeps its episode.
        assert_eq!(cats2[0].row, 2);
        assert_eq!(cats2[0].appeared, t0, "the survivor keeps its episode");
        // The new bottom twin (row 4, ordinal 1) is fresh: it plays.
        assert_eq!(cats2[1].row, 4);
        assert_eq!(cats2[1].appeared, t1, "the new bottom twin plays");
        assert!(!wd.persist[&id1].born_done, "fresh, not born-done");
        assert_eq!(
            wd.birth_seq - births_before,
            state[&"fresh"] as u64,
            "real rotation births project onto RotatePair"
        );
        let transferred_spent = cats2
            .iter()
            .filter(|o| o.appeared == t0 && wd.persist[&o.ident].nova_done)
            .count() as i64;
        let fresh_armed = cats2
            .iter()
            .filter(|o| o.appeared == t1 && !wd.persist[&o.ident].nova_done)
            .count() as i64;
        assert_eq!(transferred_spent, state[&"transferred"]);
        assert_eq!(fresh_armed, state[&"armed"]);
        assert!(model.check_invariant("NoFalseTransfers", &state));
        assert!(
            wd.persist.contains_key(&id0) && wd.persist.contains_key(&id1),
            "both live idents hold episodes"
        );
    }

    /// With no stationary anchor, a global one-row insertion transfers all
    /// three twins (the augmenting chain a greedy matcher loses).
    #[test]
    fn global_insertion_transfers_dense_triple_without_births() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut original = Terminal::new(7, 64);
        original.process(
            b"\x1b[1;1Hdear kitty friend\x1b[3;1Hdear kitty friend\x1b[5;1Hdear kitty friend",
        );
        wd.rescan(&original, 7, 64, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 3);
        for o in &wd.occ {
            wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
        }

        let mut inserted = Terminal::new(7, 64);
        inserted.process(
            b"\x1b[2;1Hdear kitty friend\x1b[4;1Hdear kitty friend\x1b[6;1Hdear kitty friend",
        );
        let births_before = wd.birth_seq;
        wd.rescan(
            &inserted,
            7,
            64,
            &lex,
            &c,
            2,
            t0 + Duration::from_millis(16),
        );
        assert_eq!(wd.birth_seq, births_before, "all three episodes transfer");
        assert_eq!(wd.occ.len(), 3);
        assert!(wd.occ.iter().all(|o| o.appeared == t0));
        assert!(wd.occ.iter().all(|o| wd.persist[&o.ident].nova_done));
    }

    /// v3 §1.1 skip semantics: old rows [0, 5] vs new [5, 20] resolve
    /// 0→unmatched, 5→5, 20→fresh (no fallback to a farther occurrence).
    #[test]
    fn alignment_skip_semantics_old_episode_goes_unmatched() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut a = Terminal::new(24, 64);
        a.process(b"dear kitty friend\r\n\r\n\r\n\r\n\r\ndear kitty friend");
        wd.rescan(&a, 24, 64, &lex, &c, 1, t0);
        // New layout: rows [5, 20].
        let mut b = Terminal::new(24, 64);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&b"\r\n".repeat(5));
        bytes.extend_from_slice(b"dear kitty friend");
        bytes.extend_from_slice(&b"\r\n".repeat(15));
        bytes.extend_from_slice(b"dear kitty friend");
        b.process(&bytes);
        let t1 = t0 + Duration::from_secs(1);
        wd.rescan(&b, 24, 64, &lex, &c, 2, t1);
        let cats: Vec<Occurrence> = wd
            .occ
            .iter()
            .filter(|o| o.class == Class::Feline)
            .cloned()
            .collect();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].row, 5);
        assert_eq!(cats[0].appeared, t0, "5→5: the row-5 episode transfers");
        assert_eq!(cats[1].row, 20);
        assert_eq!(
            cats[1].appeared, t1,
            "20 is FRESH — the unmatched row-0 episode must NOT adopt it"
        );
    }

    /// v3 §1.1: `reset()` flushes started episodes' marks so the line re-enters
    /// born-done; `hard_reset()` clears them and replays.
    #[test]
    fn reset_flush_writes_marks_and_reentry_is_born_done() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\nnice kitty");
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        // Start the one-shot (latch the phase clock, then a landed frame).
        tick_cat_at(&mut wd, t0, &c, g, None, true);
        let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
        assert!(!q.is_empty());
        // Transient reset (pane-space change): the mark is flushed, kept.
        wd.reset();
        assert_eq!(wd.done_marks.len(), 1, "the flush pass wrote the mark");
        // The same line re-enters: born-done — inert (no entrance, ever).
        wd.rescan(&term, 4, 20, &lex, &c, 2, t0 + Duration::from_secs(1));
        assert!(
            wd.persist.values().all(|e| e.born_done),
            "born-done re-entry"
        );
        let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(1100), &c, g, None, true);
        assert!(q.is_empty(), "a born-done cat emits zero quads");
        assert!(
            !wd.is_active(t0 + Duration::from_millis(1100)),
            "inert: no wakes"
        );
        // hard_reset (master toggle / config reload): marks clear — replays.
        wd.hard_reset();
        assert!(wd.done_marks.is_empty());
        wd.rescan(&term, 4, 20, &lex, &c, 3, t0 + Duration::from_secs(2));
        tick_cat_at(&mut wd, t0 + Duration::from_millis(2000), &c, g, None, true);
        let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(2100), &c, g, None, true);
        assert!(!q.is_empty(), "hard_reset makes re-ignition reachable");
    }

    /// v3 §1.1: a transferred episode that later grace-expires writes its mark
    /// under its departure ident, so the next appearance is born-done.
    #[test]
    fn transfer_then_expire_writes_mark_under_departure_ident() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut wd = WordDecorations::default();
        let mut twins = Terminal::new(6, 64);
        twins.process(b"dear kitty friend\r\n\r\ndear kitty friend");
        wd.rescan(&twins, 6, 64, &lex, &c, 1, t0);
        // Start both one-shots.
        tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
        // The row-0 twin scrolls off; the survivor's episode moves to ordinal 0.
        let mut lone = Terminal::new(6, 64);
        lone.process(b"\r\n\r\ndear kitty friend");
        wd.rescan(&lone, 6, 64, &lex, &c, 2, at(1));
        let survivor = feline(&wd).clone();
        assert_eq!(survivor.appeared, t0, "the survivor kept its episode");
        // Past GRACE_TTL blank: the mark is written under the post-transfer ident.
        let blank = Terminal::new(6, 64);
        wd.rescan(&blank, 6, 64, &lex, &c, 3, at(15));
        assert!(wd.persist.is_empty());
        assert!(!wd.done_marks.is_empty(), "expiry wrote the done marks");
        // The lone layout re-enters born-done under the transferred key.
        wd.rescan(&lone, 6, 64, &lex, &c, 4, at(16));
        assert!(
            wd.persist[&survivor.ident].born_done,
            "the re-appearance is suppressed by the transferred mark"
        );
    }

    /// v3 §1.1 resize-settle: births until a stable-cols rescan past 500 ms are
    /// born-settled and unmarked; later births play.
    #[test]
    fn resize_settle_births_are_inert_and_unmarked() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        // cols 40 on an empty grid, then a step to 32 with a new word.
        let empty40 = Terminal::new(4, 40);
        wd.rescan(&empty40, 4, 40, &lex, &c, 1, t0);
        let mut narrower = Terminal::new(4, 32);
        narrower.process(b"\r\n\r\nnice kitty");
        wd.rescan(&narrower, 4, 32, &lex, &c, 2, at(100));
        assert!(
            wd.persist.values().any(|e| e.born_settled),
            "the width-step birth is born-settled"
        );
        let (q, _, _) = tick_cat_at(&mut wd, at(200), &c, g, None, true);
        assert!(q.is_empty(), "born-settled: the entrance is skipped");
        // Departure of a born-settled episode writes no mark.
        let blank = Terminal::new(4, 32);
        wd.rescan(&blank, 4, 32, &lex, &c, 3, at(15_000));
        assert!(wd.persist.is_empty());
        assert!(
            wd.done_marks.is_empty(),
            "born-settled episodes are never written to done_marks"
        );
        // Past the settle window at stable cols: fresh births play again.
        wd.rescan(&narrower, 4, 32, &lex, &c, 4, at(16_000));
        assert!(
            wd.persist.values().all(|e| !e.born_settled && !e.born_done),
            "stable-cols births play"
        );
        tick_cat_at(&mut wd, at(16_050), &c, g, None, true); // latch the clock
        let (q, _, _) = tick_cat_at(&mut wd, at(16_250), &c, g, None, true);
        assert!(!q.is_empty(), "the post-settle birth rises");
    }

    // ───────────────────── §F4.2 Kitty Log recording ─────────────────────

    /// §F4.2: one sighting per episode, on the first frame quads land, silent
    /// across grace re-hits, and silent again for the born-done re-appearance
    /// after episode death.
    #[test]
    fn kitty_sighting_once_per_episode_across_occlusion() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let blank = Terminal::new(4, 20);
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        // At t0 the reveal is 0: nothing lands yet…
        tick_cat_at(&mut wd, t0, &c, g, None, true);
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            0,
            "reveal 0: no quads yet"
        );
        // …and the first present with landed quads records the sighting.
        tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
        let s: Vec<KittySighting> = wd.drain_kitty_sightings().collect();
        assert_eq!(s.len(), 1, "one sighting on the first landed present");
        assert_eq!(s[0].shown_as, KittyShownAs::Cat);
        assert!(!s[0].langs.is_empty(), "language attribution rides along");
        // Later frames: silent.
        tick_cat_at(&mut wd, t0 + Duration::from_millis(300), &c, g, None, true);
        assert_eq!(wd.drain_kitty_sightings().count(), 0, "once per episode");
        // Occlusion + grace re-hit: still silent (the episode persists).
        wd.rescan(&blank, 4, 20, &lex, &c, 2, at(1));
        wd.rescan(&term, 4, 20, &lex, &c, 3, at(2));
        tick_cat_at(&mut wd, at(2), &c, g, None, true);
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            0,
            "a grace re-hit never recounts"
        );
        // Grace expiry writes the done mark: the re-appearance is born-done
        // and logs nothing.
        wd.rescan(&blank, 4, 20, &lex, &c, 4, at(15));
        assert!(wd.persist.is_empty());
        wd.rescan(&term, 4, 20, &lex, &c, 5, at(16));
        tick_cat_at(
            &mut wd,
            at(16) + Duration::from_millis(100),
            &c,
            g,
            None,
            true,
        );
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            0,
            "a born-done re-appearance logs nothing (§1.1 inertness)"
        );
    }

    // ───────────────────── §6 supernova battery ─────────────────────

    fn cfg_nova() -> DecoConfig {
        DecoConfig {
            profanity_style: ProfanityStyle::Nova,
            ..cfg()
        }
    }

    /// Full-surface tick for the nova battery: every output stream + fp.
    #[allow(clippy::type_complexity, reason = "test-local 5-stream harness tuple")]
    fn tick_nova(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        cursor: Option<(u16, u16)>,
        sel: Option<SelView<'_>>,
    ) -> (
        Vec<GlowQuad>,
        Vec<WordDecoration>,
        Vec<InkCell>,
        Vec<FreeSprite>,
        u64,
    ) {
        let mut out = Vec::new();
        let mut ink = Vec::new();
        let mut fr = Vec::new();
        let mut nova = Vec::new();
        let fp = wd.tick(
            now,
            cfg,
            geom,
            cursor.map(CompanionOnGlass::at_cell),
            sel,
            true,
            &mut out,
            &mut ink,
            &mut fr,
            &mut nova,
        );
        (nova, out, ink, fr, fp)
    }

    /// The granted ignition instants of every profanity occurrence, in
    /// occurrence (row-major) order.
    fn ignition_starts(wd: &WordDecorations) -> Vec<Option<Instant>> {
        wd.occ
            .iter()
            .filter(|o| o.class == Class::Profanity)
            .map(|o| wd.persist.get(&o.ident).and_then(|e| e.nova_start))
            .collect()
    }

    /// An engine holding classic-nova profanity words (cols 4..=7, one per
    /// `(row, seed, magic, dec_line)`), each already granted at `now` —
    /// bypassing the limiter, so a test pins what happens after the grant.
    fn granted_novas(now: Instant, words: &[(u16, u64, u64, bool)]) -> WordDecorations {
        let mut wd = WordDecorations {
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        for &(row, seed, magic, dec_line) in words {
            let o = Occurrence {
                row,
                start_col: 4,
                end_col: 7,
                seed,
                ident: mix(seed),
                genome: Genome { gkey: 0, magic },
                dec_line,
                spec: class_default_spec(Class::Profanity, &cfg_nova()),
                ..occ(Class::Profanity, now)
            };
            let mut e = Episode::fresh(now, o.genome, seed, row, 0);
            e.nova_start = Some(now);
            wd.persist.insert(o.ident, e);
            wd.occ.push(o);
        }
        wd
    }

    fn select_span(row: i32, c0: u16, c1: u16) -> aterm_core::selection::TextSelection {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let mut s = aterm_core::selection::TextSelection::new();
        s.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
        s.update_selection(row, c1, SelectionSide::Right);
        s.complete_selection();
        s
    }

    /// §6.4: one flash per episode across occlusion (phase ride, then a grace
    /// re-hit never re-ignites); the re-appearance after identity death is
    /// born-done.
    #[test]
    fn nova_one_flash_per_episode_across_occlusion() {
        let lex = lex();
        let c = cfg_nova();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        // First tick grants the ignition (slot == now: the limiter is empty).
        let (q, _, _, _, _) = tick_nova(&mut wd, t0, &c, g, None, None);
        assert!(q.is_empty(), "the Dip is ink-only (no quads)");
        assert_eq!(ignition_starts(&wd), vec![Some(t0)]);
        assert!(wd.is_active(t0), "the granted window arms the scheduler");
        // Flash: the star-glint crown blooms.
        let (q, _, _, _, _) = tick_nova(&mut wd, at(180), &c, g, None, None);
        assert!(!q.is_empty(), "the flash crown emits additive quads");
        // Ring window: quads live and the fp changes every frame.
        let (q1, _, _, _, fp1) = tick_nova(&mut wd, at(500), &c, g, None, None);
        assert!(!q1.is_empty(), "ring quads");
        let (_, _, _, _, fp2) = tick_nova(&mut wd, at(516), &c, g, None, None);
        assert_ne!(fp1, fp2, "an animating nova must never be present-skipped");
        // Debris rides the existing wdeco Add stream mid-window.
        let (_, out, _, _, _) = tick_nova(&mut wd, at(700), &c, g, None, None);
        assert!(
            out.iter()
                .filter(|d| matches!(d.blend, DecoBlend::Add))
                .count()
                > 1,
            "ballistic debris motes on the Add stream, got {out:?}"
        );
        // Past the window the ember residual fades for ≤ 2 s…
        let (q, out, _, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
        assert!(q.is_empty(), "the fade emits nothing on the nova stream");
        assert_eq!(out.len(), 1, "the ember spark is fading");
        assert!(matches!(out[0].blend, DecoBlend::Add));
        assert!(wd.is_active(at(2600)), "the ember fade is animating");
        // …then zero decos, a stable fp and disarmed, forever.
        let (q, out, _, _, fpa) = tick_nova(&mut wd, at(3600), &c, g, None, None);
        assert!(q.is_empty() && out.is_empty(), "the ember faded to nothing");
        let (_, _, _, _, fpb) = tick_nova(&mut wd, at(3616), &c, g, None, None);
        assert_eq!(fpa, fpb, "the post-fade steady state is fp-stable");
        assert!(
            !wd.is_active(at(3616)),
            "a spent nova disarms the scheduler"
        );
        // Occlude one rescan (grace holds the episode), re-hit: no re-flash.
        let blank = Terminal::new(2, 20);
        wd.rescan(&blank, 2, 20, &lex, &c, 2, at(4000));
        wd.rescan(&term, 2, 20, &lex, &c, 3, at(5000));
        tick_nova(&mut wd, at(5000), &c, g, None, None);
        let (q, _, _, _, _) = tick_nova(&mut wd, at(5300), &c, g, None, None);
        assert!(
            q.is_empty(),
            "a grace re-hit must not strobe (nova_done survives)"
        );
        assert!(!wd.is_active(at(5300)));
        // Grace expiry writes the done mark, so the re-appearance is born-done:
        // no re-ignition, only the settled ink.
        wd.rescan(&blank, 2, 20, &lex, &c, 4, at(16_000));
        assert!(wd.persist.is_empty(), "grace expiry sweeps the episode");
        wd.rescan(&term, 2, 20, &lex, &c, 5, at(17_000));
        tick_nova(&mut wd, at(17_000), &c, g, None, None);
        assert_eq!(
            ignition_starts(&wd),
            vec![None],
            "born-done: the limiter is never asked"
        );
        let (q, out, ink, _, _) = tick_nova(&mut wd, at(17_300), &c, g, None, None);
        assert!(q.is_empty(), "born-done never re-ignites");
        assert!(out.is_empty(), "born-done emits no residual decos");
        assert!(
            !ink.is_empty(),
            "born-done still shows the settled ink bytes"
        );
        assert!(!wd.is_active(at(17_300)), "born-done is inert (zero wakes)");
    }

    /// Tier-1 binding for `DoneMarkLruBound`: fill, touch the oldest, replace at
    /// capacity; counters, order and constant link work match the model.
    #[test]
    fn done_mark_lru_real_order_and_constant_work_conform() {
        const CAP: usize = 3;
        const MAX_LINK_WRITES: u8 = 6;
        let model = aterm_models_effects::done_mark_lru_model();
        let mut state = model.init_state();
        let mut marks = DoneMarkLru::default();

        for key in [10u64, 20, 30] {
            let mutation = marks.insert_with_cap(key, CAP);
            assert!(model.fire("Insert", &mut state));
            assert_eq!(mutation.evicted, None);
            assert!(mutation.link_writes <= MAX_LINK_WRITES);
            assert_eq!(marks.len() as i64, state[&"resident"]);
            marks.assert_valid();
        }
        assert_eq!(marks.keys_oldest_first(), vec![10, 20, 30]);

        assert!(marks.touch(10));
        assert!(model.fire("Touch", &mut state));
        assert_eq!(marks.keys_oldest_first(), vec![20, 30, 10]);
        marks.assert_valid();

        let mutation = marks.insert_with_cap(40, CAP);
        assert!(model.fire("ReplaceOldest", &mut state));
        assert_eq!(mutation.evicted, Some(20));
        assert!(mutation.link_writes <= MAX_LINK_WRITES);
        assert_eq!(marks.keys_oldest_first(), vec![30, 10, 40]);
        assert_eq!(marks.len() as i64, state[&"resident"]);
        assert_eq!(state[&"selections"], 1);
        assert!(model.check_invariant("ConstantSelection", &state));
        marks.assert_valid();

        // Negative control: a `min_by_key` scan over every resident (the Buggy
        // model's three probes) fails the same at-cap transition.
        let mut buggy = aterm_models_effects::done_mark_lru_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut bad = buggy.init_state();
        for _ in 0..CAP {
            assert!(buggy.fire("Insert", &mut bad));
        }
        assert!(buggy.fire("ReplaceOldest", &mut bad));
        assert_eq!(bad[&"selections"], CAP as i64);
        assert!(!buggy.check_invariant("ConstantSelection", &bad));
    }

    /// At the 65,536-entry cap, thousands of replacements reuse the same slots
    /// and allocation, each within `link_writes <= 6`.
    #[test]
    fn done_mark_lru_full_capacity_replacement_is_bounded_and_growth_free() {
        const MAX_LINK_WRITES: u8 = 6;
        let mut marks = DoneMarkLru::default();
        for key in 0..DONE_MARKS_CAP as u64 {
            let mutation = marks.insert(key);
            assert_eq!(mutation.evicted, None);
            assert!(mutation.link_writes <= MAX_LINK_WRITES);
        }
        assert_eq!(marks.len(), DONE_MARKS_CAP);
        let capacities = (
            marks.index.capacity(),
            marks.nodes.capacity(),
            marks.free.capacity(),
        );
        for offset in 0..4096u64 {
            let key = DONE_MARKS_CAP as u64 + offset;
            let mutation = marks.insert(key);
            assert_eq!(mutation.evicted, Some(offset));
            assert!(mutation.link_writes <= MAX_LINK_WRITES);
            assert_eq!(marks.len(), DONE_MARKS_CAP);
        }
        // The map's reported capacity may drop with tombstones but never exceeds
        // the warmed allocation; node/free storage is fixed.
        assert!(
            marks.index.capacity() <= capacities.0,
            "replacement unexpectedly grew the warmed hash allocation"
        );
        assert_eq!(marks.nodes.capacity(), capacities.1);
        assert_eq!(marks.free.capacity(), capacities.2);
        assert_eq!(marks.nodes.len(), DONE_MARKS_CAP);
        assert!(marks.free.is_empty());
        marks.assert_valid();
    }

    fn reservation_episode(owner: u64, now: Instant) -> Episode {
        let mut episode = Episode::fresh(
            now,
            Genome {
                gkey: owner,
                magic: mix(owner),
            },
            owner,
            0,
            0,
        );
        episode.burst_kind = Some(BurstKind::Nova);
        episode.burst_roll = true;
        episode
    }

    fn reserve_overlapping(wd: &mut WordDecorations, owner: u64, now: Instant) -> Instant {
        wd.persist.insert(owner, reservation_episode(owner, now));
        let start = grant_ignition(&mut wd.ignitions, None, owner, now, (100, 100), 88.0)
            .expect("a live owner must fit the structural reservation cap");
        wd.persist
            .get_mut(&owner)
            .expect("owner resident")
            .nova_start = Some(start);
        start
    }

    fn assert_reservation_projection(
        wd: &WordDecorations,
        now: Instant,
        state: &std::collections::BTreeMap<&'static str, i64>,
    ) {
        let pending = wd
            .ignitions
            .iter()
            .filter(|reservation| reservation.start > now)
            .count() as i64;
        let recent = wd
            .ignitions
            .iter()
            .filter(|reservation| {
                reservation.start <= now && reservation.start + IGNITION_WINDOW > now
            })
            .count() as i64;
        assert_eq!(wd.persist.len() as i64, state[&"live"]);
        assert_eq!(pending, state[&"pending"]);
        assert_eq!(recent, state[&"recent"]);
        assert_eq!(wd.ignitions.len(), (pending + recent) as usize);
        assert!(wd.ignitions.len() <= MAX_IGNITION_RESERVATIONS);
    }

    /// Tier-1 binding for `IgnitionReservationLifecycle`: grants, cancelling a
    /// departed future owner, keeping fired history, and pending-to-recent
    /// migration; an expiry-only sweep matches the Buggy counterexample.
    #[test]
    fn ignition_reservation_lifecycle_real_queue_conforms() {
        let model = aterm_models_effects::ignition_reservation_lifecycle_model();
        let t0 = Instant::now();
        let mut state = model.init_state();
        let mut wd = WordDecorations::default();

        assert_eq!(reserve_overlapping(&mut wd, 1, t0), t0);
        assert!(model.fire("ReserveNow", &mut state));
        assert_eq!(reserve_overlapping(&mut wd, 2, t0), t0 + IGNITION_WINDOW);
        assert!(model.fire("ReserveFuture", &mut state));
        assert_eq!(
            reserve_overlapping(&mut wd, 3, t0),
            t0 + IGNITION_WINDOW + IGNITION_WINDOW
        );
        assert!(model.fire("ReserveFuture", &mut state));
        assert_reservation_projection(&wd, t0, &state);

        let initial = wd.ignitions.clone();

        // A future owner disappears before flashing: its slot is cancelled.
        wd.persist.remove(&2);
        assert!(model.fire("CancelFuture", &mut state));
        wd.prune_ignitions(t0);
        assert_reservation_projection(&wd, t0, &state);
        assert!(wd.ignitions.iter().all(|r| r.owner != 2));

        // A fired owner disappears: rolling-window history must remain.
        wd.persist.remove(&1);
        assert!(model.fire("DepartFired", &mut state));
        wd.prune_ignitions(t0);
        assert_reservation_projection(&wd, t0, &state);
        assert!(wd.ignitions.iter().any(|r| r.owner == 1));

        // At t+1 the fired history expires; owner 3 is still pending at t+2.
        let t1 = t0 + IGNITION_WINDOW;
        assert!(model.fire("ExpireRecent", &mut state));
        wd.prune_ignitions(t1);
        assert_reservation_projection(&wd, t1, &state);

        // A new owner takes the open slot without disturbing owner 3's.
        assert_eq!(reserve_overlapping(&mut wd, 4, t1), t1);
        assert!(model.fire("ReserveNow", &mut state));
        assert_reservation_projection(&wd, t1, &state);

        let t2 = t1 + IGNITION_WINDOW;
        wd.prune_ignitions(t2);
        assert!(model.fire("ExpireRecent", &mut state));
        assert!(model.fire("FirePending", &mut state));
        assert_reservation_projection(&wd, t2, &state);
        for invariant in [
            "FutureOwned",
            "PendingBound",
            "RecentBound",
            "ReservationBound",
        ] {
            assert!(model.check_invariant(invariant, &state), "{invariant}");
        }

        // Negative control: an expiry-only sweep keeps both ownerless slots.
        let mut buggy = aterm_models_effects::ignition_reservation_lifecycle_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut bad = buggy.init_state();
        for action in ["ReserveNow", "ReserveFuture", "ReserveFuture"] {
            assert!(buggy.fire(action, &mut bad));
        }
        assert!(buggy.fire("CancelFuture", &mut bad));
        assert!(buggy.fire("CancelFuture", &mut bad));
        let mut expiry_only = initial;
        expiry_only.retain(|reservation| reservation.start + IGNITION_WINDOW > t0);
        let ownerless_pending = expiry_only
            .iter()
            .filter(|reservation| reservation.start > t0)
            .count() as i64;
        assert_eq!(bad[&"live"], 1);
        assert_eq!(bad[&"pending"], ownerless_pending);
        assert!(!buggy.check_invariant("FutureOwned", &bad));
    }

    /// Tier-1 binding for `IgnitionReservationRekey`: a delayed nova's slot is
    /// rekeyed with its episode, survives pruning and keeps a competitor out;
    /// a persist-only rekey (negative control) double-flashes.
    #[test]
    fn ignition_reservation_rekey_real_queue_conforms() {
        let model = aterm_models_effects::ignition_reservation_rekey_model();
        let mut state = model.init_state();
        let t0 = Instant::now();
        let t1 = t0 + IGNITION_WINDOW;
        let t2 = t1 + IGNITION_WINDOW;
        let rekey_at = t0 + Duration::from_millis(100);
        let lexicon = lex();
        let config = cfg_nova();
        let mut wd = WordDecorations::default();
        let mut original = Terminal::new(2, 48);
        original.process(b"oh fuck");
        let mut moved = Terminal::new(2, 48);
        moved.process(b"status fuck");
        wd.rescan(&original, 2, 48, &lexicon, &config, 1, t0);
        let old_owner = wd.occ[0].ident;
        let mut probe = WordDecorations::default();
        probe.rescan(&moved, 2, 48, &lexicon, &config, 1, t0);
        let expected_new_owner = probe.occ[0].ident;
        assert_ne!(old_owner, expected_new_owner, "fixture must rekey");

        let mut blocker = 1u64;
        while blocker == old_owner || blocker == expected_new_owner {
            blocker += 1;
        }

        // A blocker holds t0, so the episode under test gets t1.
        assert_eq!(reserve_overlapping(&mut wd, blocker, t0), t0);
        let original_start =
            grant_ignition(&mut wd.ignitions, None, old_owner, t0, (100, 100), 88.0)
                .expect("live target fits structural cap");
        assert_eq!(original_start, t1);
        wd.persist
            .get_mut(&old_owner)
            .expect("target episode")
            .nova_start = Some(original_start);
        assert!(model.fire("GrantDelayed", &mut state));

        // A horizontal move: `align_pending` rekeys the episode and its slot.
        wd.rescan(&moved, 2, 48, &lexicon, &config, 2, rekey_at);
        let new_owner = wd.occ[0].ident;
        assert_eq!(new_owner, expected_new_owner);
        assert!(model.fire("Rekey", &mut state));
        assert_eq!(wd.persist[&new_owner].nova_start, Some(t1));
        assert!(
            wd.ignitions
                .iter()
                .any(|reservation| reservation.owner == new_owner && reservation.start == t1),
            "the future limiter slot must move with its episode"
        );

        wd.prune_ignitions(rekey_at);
        assert!(model.fire("Prune", &mut state));
        assert_eq!(wd.ignitions.len(), 2);

        // The preserved t1 slot pushes an overlapping competitor to t2.
        let mut competitor = blocker + 1;
        while wd.persist.contains_key(&competitor) {
            competitor += 1;
        }
        assert_eq!(reserve_overlapping(&mut wd, competitor, rekey_at), t2);
        assert!(model.fire("CompetingGrant", &mut state));
        for invariant in [
            "RekeyOwnsReservation",
            "DelayedSlotSurvivesPrune",
            "NoOverlappingFlash",
            "PhaseBounded",
        ] {
            assert!(model.check_invariant(invariant, &state), "{invariant}");
        }

        // Negative control: a persist-only rekey loses owner 2's future slot
        // while episode 22 still flashes at t1.
        let mut buggy = aterm_models_effects::ignition_reservation_rekey_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut bad_state = buggy.init_state();
        let mut bad = WordDecorations::default();
        assert_eq!(reserve_overlapping(&mut bad, 1, t0), t0);
        assert_eq!(reserve_overlapping(&mut bad, 2, t0), t1);
        assert!(buggy.fire("GrantDelayed", &mut bad_state));
        let episode = bad.persist.remove(&2).expect("old aligned episode");
        bad.persist.insert(22, episode);
        assert!(buggy.fire("Rekey", &mut bad_state));
        assert_eq!(bad.persist[&22].nova_start, Some(t1));

        bad.prune_ignitions(t0);
        assert!(buggy.fire("Prune", &mut bad_state));
        assert!(
            bad.ignitions
                .iter()
                .all(|reservation| reservation.owner != 2)
        );
        assert_eq!(reserve_overlapping(&mut bad, 3, t0), t1);
        assert!(buggy.fire("CompetingGrant", &mut bad_state));
        assert_eq!(bad.persist[&22].nova_start, bad.persist[&3].nova_start);
        assert!(!buggy.check_invariant("RekeyOwnsReservation", &bad_state));
        assert!(!buggy.check_invariant("DelayedSlotSurvivesPrune", &bad_state));
        assert!(!buggy.check_invariant("NoOverlappingFlash", &bad_state));
    }

    /// Ten thousand vanished delayed owners leave one fired record, and the
    /// reservation Vec stops growing after warmup.
    #[test]
    fn vanished_classic_nova_flood_has_constant_reservation_storage() {
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        assert_eq!(reserve_overlapping(&mut wd, 1, t0), t0);

        // Warm the two-entry Vec capacity once.
        assert_eq!(reserve_overlapping(&mut wd, 2, t0), t0 + IGNITION_WINDOW);
        wd.persist.remove(&2);
        wd.prune_ignitions(t0);
        let capacity = wd.ignitions.capacity();

        for owner in 3..10_003u64 {
            assert_eq!(
                reserve_overlapping(&mut wd, owner, t0),
                t0 + IGNITION_WINDOW
            );
            wd.persist.remove(&owner);
            wd.prune_ignitions(t0);
            assert_eq!(wd.ignitions.len(), 1);
        }
        assert_eq!(wd.ignitions.capacity(), capacity);
        assert_eq!(wd.ignitions[0].owner, 1);
    }

    /// §6.4: two disjoint ignitions fill the rolling second; a third 200 ms
    /// later is delayed to t0 + 1 s and still fires.
    #[test]
    fn limiter_delays_third_of_three_ignitions_deterministically() {
        let lex = lex();
        let c = cfg_nova();
        // Rows 0 / 6 / 12: center distance ≥ 120 px > 2·R_max, no overlap.
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 16,
            cols: 20,
        };
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(16, 20);
        term.process(b"fuck\r\n\r\n\r\n\r\n\r\n\r\nfuck");
        wd.rescan(&term, 16, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        assert_eq!(
            ignition_starts(&wd),
            vec![Some(t0), Some(t0)],
            "two disjoint ignitions fit one rolling second"
        );
        // The third word prints 200 ms later, 6 rows further down.
        term.process(b"\r\n\r\n\r\n\r\n\r\n\r\nfuck");
        let t1 = t0 + Duration::from_millis(200);
        wd.rescan(&term, 16, 20, &lex, &c, 2, t1);
        tick_nova(&mut wd, t1, &c, g, None, None);
        assert_eq!(
            ignition_starts(&wd),
            vec![Some(t0), Some(t0), Some(t0 + IGNITION_WINDOW)],
            "the 3rd of 3 ignitions inside 200 ms is delayed to t0 + 1 s"
        );
        // The queued (Armed) ignition keeps the scheduler alive and fires.
        assert!(wd.is_active(t1));
        let tf = t0 + IGNITION_WINDOW + Duration::from_millis(300);
        let (q, _, _, _, _) = tick_nova(&mut wd, tf, &c, g, None, None);
        assert!(
            q.iter().any(|quad| quad.row >= 10),
            "the delayed row-12 nova rings after its shifted Dip start"
        );
    }

    /// §6.4: overlapping regions tighten the limiter to one per rolling second.
    #[test]
    fn limiter_tightens_to_one_per_second_on_overlap() {
        let lex = lex();
        let c = cfg_nova();
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        // Same row, adjacent words: centers 50 px apart < 2·R_max ≥ 64 px.
        term.process(b"fuck fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        assert_eq!(
            ignition_starts(&wd),
            vec![Some(t0), Some(t0 + IGNITION_WINDOW)],
            "overlapping regions admit only 1 ignition per rolling second"
        );
    }

    /// A reused owner number gets a new limiter decision rather than aliasing a
    /// fired episode's start.
    #[test]
    fn limiter_reused_owner_label_does_not_alias_fired_episode() {
        let t0 = Instant::now();
        let t1 = t0 + IGNITION_WINDOW;
        let mut ignitions = Vec::new();
        assert_eq!(
            grant_ignition(&mut ignitions, None, 7, t0, (100, 100), 88.0),
            Some(t0)
        );
        assert_eq!(
            grant_ignition(
                &mut ignitions,
                None,
                7,
                t0 + Duration::from_millis(100),
                (100, 100),
                88.0,
            ),
            Some(t1),
            "a later episode with the same owner label still respects history"
        );
        assert_eq!(ignitions.len(), 2);
    }

    /// Tier-1 conformance for `FlashLimiter`, the WCAG 2.3.1 argument: an
    /// ignition storm through `grant_ignition` projects onto the model (grants
    /// as `Ignite`, delays as its disabled guard re-enabled by `Shift`), with
    /// `IgnitionBound` and `RegionFlashPairs` holding for disjoint and
    /// overlapping storms.
    #[test]
    fn flash_limiter_conformance_real_limiter_projects_onto_model() {
        let tick = Duration::from_millis(250);
        let t0 = Instant::now();

        // ── storm 1: disjoint regions (model scenario Overlap = 0) ──
        let m = aterm_models_effects::flash_limiter_model();
        let mut st = m.init_state();
        let mut igns = Vec::new();
        let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 88.0).expect("capacity");
        assert_eq!(a, t0, "an empty window grants immediately");
        assert!(m.fire("Ignite", &mut st), "the model admits ignition 1");
        let b = grant_ignition(&mut igns, None, 2, t0, (900, 100), 88.0).expect("capacity");
        assert_eq!(b, t0, "two disjoint ignitions fit one rolling second");
        assert!(m.fire("Ignite", &mut st), "the model admits ignition 2");
        assert!(m.check_invariant("IgnitionBound", &st));
        assert!(m.check_invariant("RegionFlashPairs", &st));
        // The third request, 200 ms in, is delayed; the model's Ignite is off.
        assert!(
            !m.action_enabled("Ignite", &st),
            "the model refuses a 3rd in-window ignition"
        );
        let c = grant_ignition(
            &mut igns,
            None,
            3,
            t0 + Duration::from_millis(200),
            (100, 700),
            88.0,
        )
        .expect("capacity");
        assert_eq!(
            c,
            t0 + IGNITION_WINDOW,
            "the real limiter delays to t0 + 1 s"
        );
        // In 250 ms model ticks, Ignite re-enables at the real granted slot.
        for k in 1u32..=4 {
            assert!(m.fire("Shift", &mut st));
            assert_eq!(
                m.action_enabled("Ignite", &st),
                t0 + tick * k >= c,
                "model re-admission at tick {k} must match the granted slot"
            );
        }
        assert!(
            m.fire("Ignite", &mut st),
            "the delayed ignition fires at its slot"
        );
        assert!(m.check_invariant("IgnitionBound", &st));
        assert!(m.check_invariant("RegionFlashPairs", &st));

        // ── storm 2: overlapping regions (model scenario Overlap = 1) ──
        let mut mo = aterm_models_effects::flash_limiter_model();
        for cst in &mut mo.consts {
            if cst.0 == "Overlap" {
                cst.1 = 1;
            }
        }
        let mut st = mo.init_state();
        let mut igns = Vec::new();
        let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 88.0).expect("capacity");
        assert_eq!(a, t0);
        assert!(mo.fire("Ignite", &mut st));
        assert!(
            !mo.action_enabled("Ignite", &st),
            "overlap tightens the model to ≤ 1 per rolling second"
        );
        // 50 px < 2·R_max = 88 px: the overlapping second ignition is pushed a
        // full window out.
        let b = grant_ignition(&mut igns, None, 2, t0, (150, 100), 88.0).expect("capacity");
        assert_eq!(
            b,
            t0 + IGNITION_WINDOW,
            "overlap: 2nd ignition delayed a full window"
        );
        for k in 1u32..=4 {
            assert!(mo.fire("Shift", &mut st));
            assert_eq!(
                mo.action_enabled("Ignite", &st),
                t0 + tick * k >= b,
                "overlap re-admission at tick {k} must match the granted slot"
            );
        }
        assert!(mo.fire("Ignite", &mut st));
        assert!(mo.check_invariant("IgnitionBound", &st));
        assert!(mo.check_invariant("RegionFlashPairs", &st));
    }

    /// Negative control: an overlap-blind limiter (`overlap_dist = 0`)
    /// reproduces `Buggy = 1, Overlap = 1`: two overlapping ignitions in one
    /// second, rejected by `IgnitionBound` and `RegionFlashPairs`.
    #[test]
    fn flash_limiter_negative_control_overlap_blind_limiter_is_buggy_trace() {
        let mut healthy = aterm_models_effects::flash_limiter_model();
        let mut buggy = aterm_models_effects::flash_limiter_model();
        for cst in &mut healthy.consts {
            if cst.0 == "Overlap" {
                cst.1 = 1;
            }
        }
        for cst in &mut buggy.consts {
            if cst.0 == "Overlap" || cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let t0 = Instant::now();
        let mut igns = Vec::new();
        // Overlapping centers 50 px apart, unseen by the blind limiter: both
        // ignite at t0.
        let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 0.0).expect("capacity");
        let b = grant_ignition(&mut igns, None, 2, t0, (150, 100), 0.0).expect("capacity");
        assert_eq!(
            (a, b),
            (t0, t0),
            "the overlap-blind limiter admits two overlapping ignitions in one second"
        );
        // The Buggy model admits the same trace step for step…
        let mut st = buggy.init_state();
        assert!(buggy.fire("Ignite", &mut st));
        assert!(
            buggy.fire("Ignite", &mut st),
            "Buggy = 1 admits the second overlapping in-window ignition"
        );
        // …and the healthy model rejects it: 2 ignitions > 1, 4 pairs > 3.
        assert!(
            !healthy.check_invariant("IgnitionBound", &st),
            "2 overlapping ignitions in one second must violate IgnitionBound"
        );
        assert!(
            !healthy.check_invariant("RegionFlashPairs", &st),
            "4 transition pairs on the shared region must violate RegionFlashPairs"
        );
    }

    /// §6.4: a selected word's nova defers ignition until deselection, and an
    /// active nova's quads drop over selected cells.
    #[test]
    fn selection_defers_ignition_and_attenuates_active_novas() {
        let lex = lex();
        let c = cfg_nova();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        // Selected: no grant, no quads, ever — however long it stays selected.
        let sel = select_span(0, 0, 19);
        let sv = SelView {
            sel: &sel,
            display_offset: 0,
        };
        tick_nova(&mut wd, t0, &c, g, None, Some(sv));
        assert_eq!(
            ignition_starts(&wd),
            vec![None],
            "no ignition while selected"
        );
        tick_nova(&mut wd, at(500), &c, g, None, Some(sv));
        assert_eq!(ignition_starts(&wd), vec![None]);
        // Deselection: the ignition queues through the limiter from now.
        tick_nova(&mut wd, at(800), &c, g, None, None);
        assert_eq!(ignition_starts(&wd), vec![Some(at(800))]);
        // An active nova's quads over freshly selected cells drop.
        let (q_free, _, _, _, _) = tick_nova(&mut wd, at(1300), &c, g, None, None);
        assert!(
            !q_free.is_empty(),
            "ring live at t = 500 ms into the window"
        );
        let (q_sel, _, _, _, _) = tick_nova(&mut wd, at(1316), &c, g, None, Some(sv));
        assert!(
            q_sel.iter().all(|quad| quad.row != 0),
            "quads over the selected row attenuate away, got {q_sel:?}"
        );
        assert!(q_sel.len() < q_free.len());
    }

    /// §6.4: under `reduced_motion` a nova is a static glint (ember tint plus
    /// one spark), byte-stable from frame 0.
    #[test]
    fn reduced_motion_nova_is_a_static_glint() {
        let lex = lex();
        let mut c = cfg_nova();
        c.reduced_motion = true;
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        let (q1, out1, ink1, _, fp1) = tick_nova(&mut wd, t0, &c, g, None, None);
        assert!(q1.is_empty(), "no quads under reduced motion, ever");
        assert_eq!(out1.len(), 1, "one static ember spark from frame 0");
        assert!(matches!(out1[0].blend, DecoBlend::Add));
        assert_eq!(
            ignition_starts(&wd),
            vec![None],
            "no ignition is ever granted"
        );
        assert!(!wd.is_active(t0), "a static glint never animates");
        // Frame-invariant: identical bytes + fp at any later instant.
        let (q2, out2, ink2, _, fp2) =
            tick_nova(&mut wd, t0 + Duration::from_secs(3), &c, g, None, None);
        assert!(q2.is_empty());
        assert_eq!(out1, out2);
        assert_eq!(ink1, ink2, "the ember-tinted static gradient holds");
        assert_eq!(fp1, fp2);
        // The spark carries the palette's ember tone.
        let occ = &wd.occ[0];
        let feats = nova_features(occ.genome.gkey);
        let (_, ember) = nova::ember_pair(nova::palette(feats.palette), None);
        // Magic variants may retint (deterministic); ordinary genomes match.
        if nova_magic(occ.genome.magic).is_none() {
            assert_eq!(out1[0].color, ember);
        }
    }

    /// §6.1/§6.4: during its nova the word's ink dips ~35 % with the sweep
    /// frozen, and the spent nova leaves the ember tint.
    #[test]
    fn nova_dip_freezes_sweep_and_embers_ink() {
        let lex = lex();
        let c = cfg_nova();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None); // grant at t0
        // Frozen: mid-window frames are byte-identical.
        let (_, _, ink_a, _, _) = tick_nova(&mut wd, at(600), &c, g, None, None);
        let (_, _, ink_b, _, _) = tick_nova(&mut wd, at(700), &c, g, None, None);
        assert_eq!(ink_a, ink_b, "the sweep is frozen during the nova window");
        // Dip: the same frozen bytes, dimmed by the §6.1 envelope.
        let (_, _, ink_dip, _, _) = tick_nova(&mut wd, at(60), &c, g, None, None);
        assert_eq!(ink_dip.len(), ink_a.len());
        for (d, s) in ink_dip.iter().zip(&ink_a) {
            let dimmed = dim_rgb(rgb3_to_u32(s.color), nova::dip_envelope(60));
            assert_eq!(rgb3_to_u32(d.color), dimmed, "dip = envelope × settled ink");
        }
        // The sparkle style at the same instants sweeps (not frozen).
        let c_v1 = cfg();
        let mut wd2 = WordDecorations::default();
        wd2.rescan(&term, 2, 20, &lex, &c_v1, 1, t0);
        let mut sweep_a = Vec::new();
        let mut sweep_b = Vec::new();
        let mut o = Vec::new();
        tick_ink(&mut wd2, at(600), &c_v1, &mut o, &mut sweep_a);
        tick_ink(&mut wd2, at(700), &c_v1, &mut o, &mut sweep_b);
        assert_ne!(
            sweep_a, sweep_b,
            "control: the v1 sweep moves between frames"
        );
        // Spent: the ink anchors shift to the ember pair.
        let (_, _, ink_ember, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
        assert_ne!(
            ink_ember, ink_a,
            "the ember tint differs from the live palette"
        );
    }

    /// §6.5 coupling is byte-deterministic, fires on a neighbor word, and
    /// holds |ΔL| ≤ 5 % against the un-pulsed control.
    #[test]
    fn coupling_pulse_is_deterministic_and_luminance_bounded() {
        let lex = lex_with_emphasis();
        let g = geom20();
        let t0 = Instant::now();
        let run = |style: ProfanityStyle| -> Vec<Vec<InkCell>> {
            let cfgv = DecoConfig {
                profanity_style: style,
                ..cfg()
            };
            let mut wd = WordDecorations::default();
            let mut term = Terminal::new(2, 40);
            term.process(b"fuck ultrathink");
            wd.rescan(&term, 2, 40, &lex, &cfgv, 1, t0);
            tick_nova(&mut wd, t0, &cfgv, g, None, None); // grant at t0
            (240..900u64)
                .step_by(10)
                .map(|ms| {
                    let (_, _, ink, _, _) = tick_nova(
                        &mut wd,
                        t0 + Duration::from_millis(ms),
                        &cfgv,
                        g,
                        None,
                        None,
                    );
                    // Keep only the neighbor word's cells (cols ≥ 5).
                    ink.into_iter().filter(|i| i.col >= 5).collect()
                })
                .collect()
        };
        let a = run(ProfanityStyle::Nova);
        let b = run(ProfanityStyle::Nova);
        assert_eq!(
            a, b,
            "coupling is a pure function of (center, t, span): replay-exact"
        );
        // Control: the sparkle style has no coupling, so the frames that differ
        // are the pulse window.
        let control = run(ProfanityStyle::Sparkle);
        let mut pulsed_frames = 0usize;
        for (fa, fc) in a.iter().zip(&control) {
            if fa == fc {
                continue;
            }
            pulsed_frames += 1;
            for (ca, cc) in fa.iter().zip(fc) {
                let dl = (relative_luminance(rgb3_to_u32(ca.color))
                    - relative_luminance(rgb3_to_u32(cc.color)))
                .abs();
                assert!(dl <= 0.05, "coupling pulse |ΔL| {dl:.4} > 5%");
            }
        }
        assert!(
            pulsed_frames > 0,
            "the ring crossing must actually pulse the neighbor"
        );
        assert!(
            pulsed_frames <= 2 * 16,
            "the pulse is one-shot (~150 ms), not the whole window: {pulsed_frames}"
        );
    }

    /// §6.1: the Singularity's Over-blend RingArc reaches the host tick's
    /// output streams, capped, and `profanity.magic = false` turns it off.
    #[test]
    fn singularity_rides_the_host_tick() {
        const SING_MAGIC: u64 = 8 << 12; // → 8 → Singularity
        let now = Instant::now();
        let g = geom20();
        let run = |magic: u64, profanity_magic: bool| {
            let mut wd = granted_novas(now, &[(2, 0xF00D, magic, false)]);
            let mut c = cfg_nova();
            c.profanity_magic = profanity_magic;
            tick_nova(&mut wd, now + Duration::from_millis(400), &c, g, None, None)
        };
        // Singularity: the darkening ring rides the Over wdeco stream.
        let (_, out, _, _, _) = run(SING_MAGIC, true);
        assert!(
            out.iter().any(
                |d| matches!(d.glyph, DecoGlyph::RingArc) && matches!(d.blend, DecoBlend::Over)
            ),
            "the Singularity emits RingArc Over decos, got {out:?}"
        );
        assert!(
            out.iter()
                .filter(|d| matches!(d.glyph, DecoGlyph::RingArc))
                .count()
                <= nova::MAX_RING_ARC_CELLS
        );
        // `profanity.magic = false` pins the ordinary build: no RingArc.
        let (_, out, _, _, _) = run(SING_MAGIC, false);
        assert!(!out.iter().any(|d| matches!(d.glyph, DecoGlyph::RingArc)));
    }

    /// §6.3: of four concurrent novas the row-major last skips to Ember.
    #[test]
    fn excess_concurrent_novas_skip_to_ember() {
        let now = Instant::now();
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 30,
            cols: 20,
        };
        // All four pre-granted: this pins the concurrency cap, not the rate.
        let words: Vec<_> = (0..4u16)
            .map(|k| (k * 7, 0xF00D + u64::from(k), (100 << 12) | 100, false))
            .collect();
        let mut wd = granted_novas(now, &words);
        let at = now + Duration::from_millis(400);
        tick_nova(&mut wd, at, &cfg_nova(), g, None, None);
        let spent: Vec<bool> = wd
            .occ
            .iter()
            .map(|o| wd.persist[&o.ident].nova_done)
            .collect();
        assert_eq!(
            spent,
            vec![false, false, false, true],
            "the 4th concurrent nova (row-major last) skips straight to Ember"
        );
    }

    /// §10/§12: on a DECDWL row the nova anchors via the row advance (2× cell
    /// width), centring over the glyphs the user sees.
    #[test]
    fn dec_rows_anchor_nova_via_row_advance() {
        let now = Instant::now();
        let g = geom20();
        let run = |dec_line: bool| {
            let mut wd = granted_novas(now, &[(2, 0xF00D, (100 << 12) | 100, dec_line)]);
            let (q, _, _, _, _) = tick_nova(
                &mut wd,
                now + Duration::from_millis(400),
                &cfg_nova(),
                g,
                None,
                None,
            );
            assert!(
                !q.is_empty(),
                "novas render on DEC rows too (dec={dec_line})"
            );
            // The ring is x-symmetric around the anchor: its quads' mean
            // midpoint recovers the center.
            q.iter()
                .map(|quad| f64::from(quad.x) + f64::from(quad.w) / 2.0)
                .sum::<f64>()
                / q.len() as f64
        };
        let normal = run(false);
        let dec = run(true);
        // Cols 4..=7: center 60 px at cw 10, 120 px at the DECDWL advance.
        assert!(
            (normal - 60.0).abs() < 8.0,
            "single-width anchor ≈ 60 px, got {normal}"
        );
        assert!(
            (dec - 120.0).abs() < 8.0,
            "DECDWL anchor rides the row advance, got {dec}"
        );
    }

    // ───────────────── v3 §6 spec framework + §3.1/§3.2 battery ─────────────────

    /// Rainbow profanity with supernova chance `chance`.
    fn cfg_rainbow(chance: u8) -> DecoConfig {
        DecoConfig {
            profanity_style: ProfanityStyle::Rainbow,
            supernova_chance: chance,
            ..cfg()
        }
    }

    /// Parse + build a `[[sparkle_words.custom]]` fragment (table + the
    /// synthesized emphasis lexicon entries).
    fn custom_table(frag: &str) -> (SpecTable, String) {
        let entries = crate::spec::parse_custom_toml(frag).expect("fragment parses");
        crate::spec::build_custom(&entries)
    }

    fn lex_with_customs(frag: &str) -> (DecoConfig, Lexicon) {
        let (table, lex_frag) = custom_table(frag);
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("synthesized override parses");
        let mut c = cfg();
        c.spec_table = table;
        (c, lexicon)
    }

    /// §6: a custom spec on a builtin profanity form fires with
    /// `[profanity] enabled = false` and beats the class default.
    #[test]
    fn custom_on_builtin_profanity_fires_with_class_disabled() {
        let (mut c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"fuck\"]\nink = { colorway = \"rainbow\" }\n",
        );
        c.profanity = false;
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "the override bypasses the class gate");
        assert_eq!(wd.occ[0].class, Class::Profanity, "still a profanity match");
        assert!(
            wd.occ[0].custom,
            "the custom spec wins over the class default"
        );
        let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert!(!ink.is_empty(), "the custom rainbow renders");
        // Negative control: without the override the gate stands.
        let mut off = cfg();
        off.profanity = false;
        let mut wd2 = WordDecorations::default();
        wd2.rescan(&term, 2, 20, &lex, &off, 1, t0);
        assert!(wd2.occ.is_empty(), "profanity off + no override = no scan");
    }

    /// §6: a graphic-only custom word with ink disabled still scans and peeks.
    #[test]
    fn graphic_only_custom_word_with_ink_off_shows_a_cat() {
        let (mut c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\ngraphic = { collection = \"cats\" }\n",
        );
        c.ink_enabled = false;
        c.emphasis = true; // the resolver's folded §6 gate (has_custom)
        let t0 = Instant::now();
        let g = geom20();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(6, 20);
        term.process(b"\r\n\r\n\r\nultrathink");
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        assert_eq!(
            wd.occ.len(),
            1,
            "graphic-only custom word scans with ink off"
        );
        assert!(wd.occ[0].spec.graphic.is_some() && wd.occ[0].spec.ink.is_none());
        // The phase clock latched at birth (the rescan); mid-rise emits.
        let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, g, None, None);
        assert!(ink.is_empty(), "no ink axis, no ink");
        let (_, _, _, cats, _) =
            tick_nova(&mut wd, t0 + Duration::from_millis(300), &c, g, None, None);
        assert!(!cats.is_empty(), "the cat graphic rises on a custom word");
    }

    /// §6 custom surfaces the resolver must key: a 2-char word (explicit
    /// config is consent, so the short-surface guard is exempt), the possessive
    /// form (the four possessive-variant hashes) and a no-space-script (CJK)
    /// compound (the synthesized `cjk = true` entry and its raw form hash).
    /// Each scans exactly once, resolves the custom spec and styles as rainbow.
    #[test]
    fn custom_short_possessive_and_cjk_surfaces_scan_and_style() {
        for (word, text, cols, ink_cells) in [
            ("gg", "gg well played", 20usize, Some(2usize)),
            ("ultrathink", "ultrathink's power", 40, None),
            ("\u{732b}\u{795e}", "これは猫神です", 30, None),
        ] {
            let (c, lex) = lex_with_customs(&format!(
                "[[sparkle_words.custom]]\nwords = [\"{word}\"]\nink = {{ colorway = \"rainbow\" }}\n"
            ));
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            let mut term = Terminal::new(2, cols as u16);
            term.process(text.as_bytes());
            wd.rescan(&term, 2, cols, &lex, &c, 1, t0);
            assert_eq!(wd.occ.len(), 1, "{text:?}: the custom surface scans");
            assert!(wd.occ[0].custom, "{text:?}: the override spec resolved");
            assert!(
                matches!(
                    wd.occ[0].spec.ink,
                    Some(InkSpec {
                        colorway: Colorway::Rainbow { .. },
                        ..
                    })
                ),
                "{text:?}: styled as the custom rainbow"
            );
            if let Some(n) = ink_cells {
                let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
                assert_eq!(ink.len(), n, "{text:?}: every lead cell inks");
            }
        }
    }

    /// §3.1: past the freeze, ink and fingerprint are identical on both themes,
    /// `is_active` is false, and a done-born rainbow matches a settled one.
    #[test]
    fn rainbow_freeze_is_byte_stable_on_both_themes_and_done_born() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let build = |bg: [u8; 3], fg: [u8; 3], inert: bool| {
            let mut o = occ(Class::Profanity, t0);
            o.spec = class_default_spec(Class::Profanity, &cfg_rainbow(0));
            o.ink_cells = 4;
            o.ink_bg = bg;
            o.inert = inert;
            let mut wd = WordDecorations {
                occ: vec![o],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            wd.ink_base_fg = vec![fg; 4];
            wd.ink_cols = vec![2, 3, 4, 5];
            wd
        };
        let c = cfg_rainbow(0);
        for (bg, fg, label) in [
            ([0u8, 0, 0], [208u8, 208, 208], "dark"),
            ([255, 255, 255], [40, 40, 40], "light"),
        ] {
            let mut wd = build(bg, fg, false);
            let (_, _, ink0, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
            assert_eq!(ink0.len(), 4, "{label}: rainbow ink lands");
            let (_, _, ink_mid, _, _) = tick_nova(&mut wd, at(1200), &c, geom20(), None, None);
            assert_ne!(ink0, ink_mid, "{label}: the drift actually animates");
            assert!(wd.is_active(at(1200)), "{label}: animating while t < drift");
            let (_, _, ink_a, _, fp_a) = tick_nova(&mut wd, at(2600), &c, geom20(), None, None);
            let (_, _, ink_b, _, fp_b) = tick_nova(&mut wd, at(2700), &c, geom20(), None, None);
            assert_eq!(ink_a, ink_b, "{label}: settled bytes identical");
            assert_eq!(
                fp_a, fp_b,
                "{label}: settled fp identical (frame fold gated)"
            );
            assert!(!ink_a.is_empty(), "{label}: settled ink is non-empty");
            assert!(
                !wd.is_active(at(2600)),
                "{label}: is_active false with settled ink (the gate-hit path)"
            );
            assert_eq!(
                ink0, ink_a,
                "{label}: EXACT full cycle — frozen phase == t = 0 phase"
            );
            // Done-born twin: inert from birth ⇒ exactly the settled bytes.
            let mut wd_done = build(bg, fg, true);
            let (_, _, ink_d, _, _) = tick_nova(&mut wd_done, at(50), &c, geom20(), None, None);
            assert_eq!(ink_d, ink_a, "{label}: done-born == naturally-settled");
            assert!(!wd_done.is_active(at(50)), "{label}: born-done never arms");
        }
        // The two themes actually branch (s/v differ on light bg).
        let mut dark = build([0, 0, 0], [208, 208, 208], false);
        let mut light = build([255, 255, 255], [208, 208, 208], false);
        let (_, _, ink_dark, _, _) = tick_nova(&mut dark, at(2600), &c, geom20(), None, None);
        let (_, _, ink_light, _, _) = tick_nova(&mut light, at(2600), &c, geom20(), None, None);
        assert_ne!(
            ink_dark, ink_light,
            "theme-aware s/v: light bg takes deep candy tones"
        );
    }

    /// §3.2 roll: decorrelated across appearances, never at chance 0, always
    /// at 100, and transferred by alignment.
    #[test]
    fn supernova_roll_decorrelates_and_transfers_across_alignment() {
        let lex = lex();
        let t0 = Instant::now();
        // chance = 100: always rolls; the decision transfers with alignment.
        let c = cfg_rainbow(100);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(6, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        let id0 = wd.occ[0].ident;
        assert!(wd.persist[&id0].burst_roll, "chance = 100 always escalates");
        // Two rows lower, same column: alignment moves the episode and roll.
        term.process(b"\x1b[2J\x1b[3;1Hoh fuck");
        wd.rescan(&term, 6, 20, &lex, &c, 2, t0 + Duration::from_millis(100));
        assert_eq!(wd.occ[0].appeared, t0, "the episode transferred");
        assert!(
            wd.persist[&wd.occ[0].ident].burst_roll,
            "the decision transfers across alignment"
        );
        // chance = 0 disables the roll outright.
        let c0 = cfg_rainbow(0);
        let mut wd0 = WordDecorations::default();
        wd0.rescan(&term, 6, 20, &lex, &c0, 1, t0);
        assert!(!wd0.persist[&wd0.occ[0].ident].burst_roll);
        // Repeated births of the same word (hard resets keep `birth_seq`) see
        // both outcomes at 50%.
        let c50 = cfg_rainbow(50);
        let mut seen = [false; 2];
        for e in 0..30u64 {
            wd.hard_reset();
            wd.rescan(&term, 6, 20, &lex, &c50, 10 + e, t0);
            seen[usize::from(wd.persist[&wd.occ[0].ident].burst_roll)] = true;
        }
        assert!(
            seen[0] && seen[1],
            "independent outcomes across rebirths of the same context"
        );
    }

    /// The burst mutex holds for the supernova's full window even while its
    /// word is scrolled off (`super_until` comes from the persist-wide scan):
    /// a visible classic nova defers throughout.
    #[test]
    fn scrolled_off_supernova_window_still_defers_classic_grants() {
        let (table, lex_frag) = custom_table(
            "[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n",
        );
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("override parses");
        let mut c = cfg_rainbow(100);
        c.spec_table = table;
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let g = EffectGeom {
            rows: 8,
            ..geom20()
        };
        let mut term = Terminal::new(8, 20);
        term.process(b"fuck");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        let fuck = wd.occ[0].ident;
        assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "supernova granted");
        // Mid-window the word scrolls off; the mutex holds on grace.
        let mut term2 = Terminal::new(8, 20);
        term2.process(b"\r\nboom");
        wd.rescan(&term2, 8, 20, &lexicon, &c, 2, at(300));
        assert!(
            wd.persist.contains_key(&fuck),
            "the scrolled-off episode lives on grace"
        );
        let boom = wd
            .occ
            .iter()
            .find(|o| o.spec.burst.map(|b| b.kind) == Some(BurstKind::Nova))
            .expect("the classic word scanned")
            .ident;
        tick_nova(&mut wd, at(300), &c, g, None, None);
        assert!(
            wd.persist[&boom].nova_start.is_none(),
            "classic grant deferred behind the INVISIBLE supernova window"
        );
        assert!(
            wd.is_active(at(300)),
            "the deferred grant keeps the scheduler armed past the window"
        );
        tick_nova(&mut wd, at(1200), &c, g, None, None);
        assert!(
            wd.persist[&boom].nova_start.is_none(),
            "still deferred mid-window while scrolled off"
        );
        // Back on-screen mid-window: the classic contributed nothing, so the
        // supernova has the whole quad channel.
        let mut term3 = Terminal::new(8, 20);
        term3.process(b"fuck\r\nboom");
        wd.rescan(&term3, 8, 20, &lexicon, &c, 3, at(1500));
        let (q, ..) = tick_nova(&mut wd, at(1500), &c, g, None, None);
        assert!(!q.is_empty(), "the supernova resumes emission on revisit");
        assert!(
            wd.persist[&boom].nova_start.is_none(),
            "the classic still waits with the word back on-screen"
        );
        // Past the window: the mutex releases and the classic grant lands.
        tick_nova(
            &mut wd,
            at(supernova::SUPER_TOTAL_MS + 100),
            &c,
            g,
            None,
            None,
        );
        assert!(
            wd.persist[&fuck].nova_done,
            "the window expired into nova_done"
        );
        assert!(
            wd.persist[&boom].nova_start.is_some(),
            "granted once the supernova window ended"
        );
    }

    /// §3.2b: five typed f-bombs make the fifth a certain Nuke even at a 1%
    /// chance, and the streak decays on its own.
    #[test]
    fn fbomb_combo_five_typed_fucks_go_nuclear_then_decay() {
        let lexicon = lex();
        let c = cfg_rainbow(1);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 40);
        // One rescan per char, so each completed word is a live caret
        // completion and charges the streak.
        let mut epoch = 1u64;
        for (i, ch) in b"fuck fuck fuck fuck fuck".iter().enumerate() {
            term.process(&[*ch]);
            // Each char is a committed key: the typed witness needs it as well
            // as the caret position.
            wd.note_typed_edit(at(100 * (i as u64 + 1)), None);
            wd.rescan(&term, 4, 40, &lexicon, &c, epoch, at(100 * (i as u64 + 1)));
            epoch += 1;
        }
        assert_eq!(wd.occ.len(), 5);
        assert_eq!(wd.combo_births.len(), 5, "five typed f-bombs charged");
        let ep5 = &wd.persist[&wd.occ[4].ident];
        assert!(
            ep5.burst_roll,
            "level 5 detonates even at a 1% configured chance"
        );
        assert_eq!(
            ep5.burst_tier,
            supernova::SuperTier::Nuke,
            "level 5 is EXTREME: the mushroom cloud, guaranteed"
        );
        // The chance ladder is already certain one level down.
        let ep4 = &wd.persist[&wd.occ[3].ident];
        assert_eq!(ep4.burst_tier, supernova::SuperTier::Nuke,);
        // 31 s of silence ages every entry out: a fresh streak of one.
        for (i, ch) in b" fuck".iter().enumerate() {
            term.process(&[*ch]);
            // A committed key per char.
            wd.note_typed_edit(at(2400 + 31_000 + 100 * (i as u64 + 1)), None);
            wd.rescan(
                &term,
                4,
                40,
                &lexicon,
                &c,
                epoch,
                at(2400 + 31_000 + 100 * (i as u64 + 1)),
            );
            epoch += 1;
        }
        assert_eq!(wd.occ.len(), 6);
        assert_eq!(
            wd.combo_births.len(),
            1,
            "the streak decayed; only the fresh f-bomb remains"
        );
    }

    /// §3.2b: output f-bombs (caret elsewhere) never charge the streak.
    #[test]
    fn fbomb_combo_output_fucks_do_not_charge() {
        let lexicon = lex();
        let c = cfg_rainbow(100);
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(8, 40);
        term.process(b"fuck fuck fuck fuck fuck\r\n");
        wd.rescan(&term, 8, 40, &lexicon, &c, 1, t0);
        assert_eq!(wd.occ.len(), 5);
        assert!(
            wd.combo_births.is_empty(),
            "output f-bombs ride the ambient level but never feed it"
        );
        for occ in &wd.occ {
            assert!(
                wd.persist[&occ.ident].burst_roll,
                "chance 100 still rolls at the cold level"
            );
        }
    }

    /// §3.2b: combo heat decays in engine-clock time across a freeze (the ring
    /// is window-wide, `frozen_at` per pane): cold after a 60 s freeze.
    #[test]
    fn fbomb_combo_cools_across_freeze_thaw() {
        let lexicon = lex();
        let c = cfg_rainbow(1);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 40);
        let mut epoch = 1u64;
        for (i, ch) in b"fuck fuck fuck".iter().enumerate() {
            term.process(&[*ch]);
            // A committed key per char.
            wd.note_typed_edit(at(100 * (i as u64 + 1)), None);
            wd.rescan(&term, 4, 40, &lexicon, &c, epoch, at(100 * (i as u64 + 1)));
            epoch += 1;
        }
        assert_eq!(wd.combo_births.len(), 3);
        wd.freeze(at(2_000));
        wd.thaw(at(62_000));
        for (i, ch) in b" fuck".iter().enumerate() {
            term.process(&[*ch]);
            // A committed key per char.
            wd.note_typed_edit(at(62_100 + 100 * (i as u64 + 1)), None);
            wd.rescan(
                &term,
                4,
                40,
                &lexicon,
                &c,
                epoch,
                at(62_100 + 100 * (i as u64 + 1)),
            );
            epoch += 1;
        }
        assert_eq!(
            wd.combo_births.len(),
            1,
            "60 frozen seconds age the streak out; only the fresh f-bomb remains"
        );
    }

    /// §3.2b: the fifth typed f-bomb's Nuke puts cloud sprites on the free lane.
    #[test]
    fn fbomb_combo_nuke_cloud_reaches_the_free_lane() {
        let lexicon = lex();
        let c = cfg_rainbow(100);
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 20,
            cols: 40,
        };
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(20, 40);
        term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
        for (i, ch) in b"fuck fuck fuck fuck fuck".iter().enumerate() {
            term.process(&[*ch]);
            // A committed key per char.
            wd.note_typed_edit(at(60 * (i as u64 + 1)), None);
            wd.rescan(
                &term,
                20,
                40,
                &lexicon,
                &c,
                i as u64 + 1,
                at(60 * (i as u64 + 1)),
            );
        }
        let ep5 = &wd.persist[&wd.occ[4].ident];
        assert_eq!(ep5.burst_tier, supernova::SuperTier::Nuke);
        // With no feline words, any free sprite in a nuke window is the cloud.
        let mut cloud_frames = 0u32;
        for ms in (1500..20_000).step_by(100) {
            let (.., fr, _) = tick_nova(&mut wd, at(ms), &c, g, None, None);
            if !fr.is_empty() {
                cloud_frames += 1;
            }
        }
        assert!(
            cloud_frames >= 10,
            "the guaranteed Nuke never put its cloud on the free lane \
             (saw {cloud_frames} sprite-bearing frames)"
        );
    }

    /// §3.2b: a Nuke holds the mutex (deferring classic grants) for its full
    /// 3600 ms cloud; a Flash keeps the classic 2400 ms.
    #[test]
    fn nuke_window_holds_the_mutex_for_its_full_cloud() {
        let (table, lex_frag) = custom_table(
            "[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n",
        );
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("override parses");
        let mut c = cfg_rainbow(100);
        c.spec_table = table;
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let g = EffectGeom {
            rows: 8,
            ..geom20()
        };
        let mut term = Terminal::new(8, 20);
        term.process(b"fuck\r\nboom");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
        let fuck = wd.occ[0].ident;
        let boom = wd.occ[1].ident;
        // Force the tier: the window must follow the stored tier.
        {
            let ep = wd.persist.get_mut(&fuck).expect("fresh episode");
            ep.burst_roll = true;
            ep.burst_tier = supernova::SuperTier::Nuke;
        }
        tick_nova(&mut wd, t0, &c, g, None, None);
        assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "cloud granted");
        // Past the flat 2400 ms window: the cloud's tail is real window.
        tick_nova(
            &mut wd,
            at(supernova::SUPER_TOTAL_MS + 200),
            &c,
            g,
            None,
            None,
        );
        assert!(
            !wd.persist[&fuck].nova_done,
            "the cloud's 2400-3600 ms tail must not be truncated"
        );
        assert!(
            wd.persist[&boom].nova_start.is_none(),
            "the mutex holds through the full cloud"
        );
        // Past the nuke window: expiry and the deferred classic grant.
        tick_nova(
            &mut wd,
            at(crate::nuke::NUKE_TOTAL_MS + 100),
            &c,
            g,
            None,
            None,
        );
        assert!(
            wd.persist[&fuck].nova_done,
            "the cloud expired at ITS window"
        );
        assert!(
            wd.persist[&boom].nova_start.is_some(),
            "the classic grant lands after the cloud"
        );

        // Only the Nuke extends occupancy: a Flash frees the mutex at the
        // classic 2400 ms.
        let mut term = Terminal::new(8, 20);
        term.process(b"fuck\r\nboom");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
        let fuck = wd.occ[0].ident;
        let boom = wd.occ[1].ident;
        {
            let ep = wd.persist.get_mut(&fuck).expect("fresh episode");
            ep.burst_roll = true;
            ep.burst_tier = supernova::SuperTier::Flash;
        }
        tick_nova(&mut wd, t0, &c, g, None, None);
        assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "flash granted");
        tick_nova(&mut wd, at(1_200), &c, g, None, None);
        assert!(
            !wd.persist[&fuck].nova_done,
            "a Flash presents the classic window, mid-window it is still live"
        );
        tick_nova(
            &mut wd,
            at(supernova::SUPER_TOTAL_MS + 100),
            &c,
            g,
            None,
            None,
        );
        assert!(
            wd.persist[&fuck].nova_done,
            "a Flash expires at the classic 2400 ms window"
        );
        assert!(
            wd.persist[&boom].nova_start.is_some(),
            "the classic grant lands as soon as the Flash frees the mutex"
        );
    }

    /// The live burst windows of the bound pane and every parked shard,
    /// projected onto `SupernovaBurstMutex` through `burst_mutex_end`.
    fn burst_mutex_projection(
        wd: &WordDecorations,
        now: Instant,
    ) -> std::collections::BTreeMap<&'static str, i64> {
        let (mut supers, mut classics) = (0i64, 0i64);
        for ep in wd
            .persist
            .values()
            .chain(wd.parked.values().flat_map(|p| p.persist.values()))
        {
            if burst_mutex_end(ep).is_some_and(|end| now < end) {
                match ep.burst_kind {
                    Some(BurstKind::SuperNova) => supers += 1,
                    _ => classics += 1,
                }
            }
        }
        let funded = supers * supernova::S_MAX_BOUND as i64 + classics * 392;
        [
            ("supers", supers),
            ("classics", classics),
            ("funded", funded),
        ]
        .into_iter()
        .collect()
    }

    /// Tier-1 binding for `SupernovaBurstMutex`: every real grant across a bound
    /// and a parked pane is enabled in the model at its state (engine pass
    /// order), and every state satisfies the invariants.
    #[test]
    fn real_burst_grants_conform_to_the_supernova_mutex_model() {
        let model = aterm_models_effects::supernova_burst_mutex_model();
        let check = |wd: &mut WordDecorations,
                     now: Instant,
                     c: &DecoConfig,
                     g: EffectGeom,
                     what: &str|
         -> std::collections::BTreeMap<&'static str, i64> {
            let mut state = burst_mutex_projection(wd, now);
            tick_nova(wd, now, c, g, None, None);
            let after = burst_mutex_projection(wd, now);
            for (action, var) in [("IgniteSuper", "supers"), ("IgniteClassic", "classics")] {
                for _ in state[var]..after[var] {
                    assert!(
                        model.action_enabled(action, &state),
                        "{what}: the engine granted {action} at {state:?}, which the \
                         mutex model does not enable"
                    );
                    assert!(model.fire(action, &mut state), "{what}: {action}");
                }
            }
            assert_eq!(state, after, "{what}: a tick only grants (time retires)");
            let env = model.eval_env(&after);
            for inv in &model.invariants {
                assert!(
                    model.check_invariant_in(inv, &env),
                    "{what}: {} violated by {after:?}",
                    inv.name
                );
            }
            after
        };

        let (table, lex_frag) = custom_table(
            "[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n",
        );
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("override parses");
        let mut c = cfg_rainbow(100);
        c.spec_table = table;
        let g = EffectGeom {
            rows: 8,
            ..geom20()
        };
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // Pane 1: two supernova words and a classic one; one supernova holds.
        let mut wd = WordDecorations::default();
        wd.bind_pane(1, (0, 0));
        let mut term_a = Terminal::new(8, 20);
        term_a.process(b"fuck\r\nfuck it\r\nboom");
        wd.rescan(&term_a, 8, 20, &lexicon, &c, 1, t0);
        let s = check(&mut wd, t0, &c, g, "pane 1 first tick");
        assert_eq!(
            (s["supers"], s["classics"]),
            (1, 0),
            "one supernova holds the mutex"
        );
        let win = |wd: &WordDecorations| {
            wd.persist
                .values()
                .chain(wd.parked.values().flat_map(|p| p.persist.values()))
                .filter_map(burst_mutex_end)
                .max()
                .expect("a live window")
        };
        let first_end = win(&wd);

        // Park pane 1 mid-window; pane 2 has a supernova and a classic of its own.
        wd.bind_pane(2, (400, 0));
        let mut term_b = Terminal::new(8, 20);
        term_b.process(b"oh fuck\r\nboom");
        wd.rescan(&term_b, 8, 20, &lexicon, &c, 1, at(300));
        let s = check(&mut wd, at(300), &c, g, "pane 2 behind a PARKED supernova");
        assert_eq!(
            (s["supers"], s["classics"]),
            (1, 0),
            "both of pane 2's grants defer"
        );

        // The parked window ends: pane 2's supernova takes the mutex first.
        let t1 = first_end + Duration::from_millis(100);
        let s = check(&mut wd, t1, &c, g, "pane 2 after the parked window");
        assert_eq!(
            (s["supers"], s["classics"]),
            (1, 0),
            "pane 2's supernova granted"
        );
        let second_end = win(&wd);

        // Past pane 2's supernova: the classic finally ignites.
        let t2 = second_end + Duration::from_millis(100);
        let s = check(&mut wd, t2, &c, g, "pane 2's classic after its supernova");
        assert_eq!(
            (s["supers"], s["classics"]),
            (0, 1),
            "the mutex released the nova"
        );

        // Negative control: the forbidden grant shapes are ones the model
        // rejects.
        let state = |supers: i64, classics: i64| {
            [
                ("supers", supers),
                ("classics", classics),
                ("funded", supers * 900 + classics * 392),
            ]
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert!(
            !model.action_enabled("IgniteSuper", &state(1, 0)),
            "a second supernova"
        );
        assert!(
            !model.action_enabled("IgniteSuper", &state(0, 1)),
            "a supernova beside a classic"
        );
        assert!(
            !model.action_enabled("IgniteClassic", &state(1, 0)),
            "a classic beside a supernova"
        );
    }

    /// §3.2: wash rows split around the selected span.
    #[test]
    fn supernova_wash_splits_around_selection() {
        let lex = lex();
        let c = cfg_rainbow(100);
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 8,
            cols: 24,
        };
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(8, 24);
        term.process(b"\r\n\r\n\r\nfuck");
        wd.rescan(&term, 8, 24, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None); // grant (no selection yet)
        let sel = select_span(1, 8, 11);
        let sv = SelView {
            sel: &sel,
            display_offset: 0,
        };
        let (q, ..) = tick_nova(
            &mut wd,
            t0 + Duration::from_millis(500),
            &c,
            g,
            None,
            Some(sv),
        );
        assert!(!q.is_empty(), "mid-detonation wash present");
        // Wash pieces on the selected row split into left + right, ≤ 3 per row.
        let wash1: Vec<&GlowQuad> = q
            .iter()
            .filter(|x| x.row == 1 && i32::from(x.w) > 4 * 10)
            .collect();
        assert!(
            wash1.len() >= 2,
            "the full-width wash row split into remainders: {wash1:?}"
        );
        assert!(
            wash1.len() <= 3,
            "≤ 3 wash quads/row post-split: {}",
            wash1.len()
        );
        // Every quad on the row (wash + crown) clears the selected span.
        for x in q.iter().filter(|x| x.row == 1) {
            let (x0, x1) = (i32::from(x.x), i32::from(x.x) + i32::from(x.w));
            assert!(
                x1 <= 80 || x0 >= 120,
                "no quad overlaps the selected px span [80, 120): {x:?}"
            );
        }
        // Splitting duplicates a wash color into disjoint pieces, so check per
        // pixel that the post-split stream never exceeds the emitter's cap.
        for row in 0..g.rows {
            for px in 0..u32::from(g.cols) * u32::from(g.cell_w) {
                let y0 = u32::from(row) * u32::from(g.cell_h);
                for py in y0..y0 + u32::from(g.cell_h) {
                    let mut sum = [0u32; 3];
                    for quad in q.iter().filter(|quad| {
                        quad.row == row
                            && px >= u32::from(quad.x)
                            && px < u32::from(quad.x) + u32::from(quad.w)
                            && py >= u32::from(quad.y)
                            && py < u32::from(quad.y) + u32::from(quad.h)
                    }) {
                        sum[0] += (quad.color >> 16) & 0xff;
                        sum[1] += (quad.color >> 8) & 0xff;
                        sum[2] += quad.color & 0xff;
                    }
                    let peak = sum.into_iter().max().unwrap_or(0);
                    assert!(
                        peak <= supernova::MAX_VIEWPORT_OVERLAY,
                        "post-split ({px}, {py}) accumulates {peak} channel levels"
                    );
                }
            }
        }
    }

    /// §3.2: `reduced_motion` suppresses the supernova (static rainbow only).
    #[test]
    fn supernova_suppressed_under_reduced_motion() {
        let lex = lex();
        let mut c = cfg_rainbow(100);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        let (q, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert!(q.is_empty(), "no supernova quads under reduced_motion");
        assert!(
            wd.persist[&wd.occ[0].ident].nova_start.is_none(),
            "no ignition granted"
        );
        assert!(!ink.is_empty(), "the static rainbow still inks");
        assert!(
            !wd.is_active(t0 + Duration::from_millis(1)),
            "static = idle"
        );
    }

    // ──────────── one-shot × reduced-motion regression battery ────────────

    /// A completed one-shot (cat, paw, nova ember, sparkle residual) does not
    /// resurrect under `reduced_motion` (unfocused demotion) or on refocus.
    #[test]
    fn full_play_then_reduce_resurrects_nothing() {
        let lex = lex();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let reduce = |c: &DecoConfig| {
            let mut r = c.clone();
            r.reduced_motion = true;
            r
        };
        // Cat (row 2, head on-screen): play to Done, then reduce: no sprites.
        let c = cfg();
        let mut cat_term = Terminal::new(4, 20);
        cat_term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&cat_term, 4, 20, &lex, &c, 1, t0);
        tick_cat_at(&mut wd, t0, &c, g, None, true); // first present (clock latched at birth)
        let (fr, _, _) = tick_cat_at(&mut wd, at(100), &c, g, None, true);
        assert!(!fr.is_empty(), "the cat is rising");
        let (fr, out, _) = tick_cat_at(&mut wd, at(6_000), &c, g, None, true);
        assert!(fr.is_empty() && out.is_empty(), "the one-shot completed");
        let (fr, out, _) = tick_cat_at(&mut wd, at(6_050), &reduce(&c), g, None, true);
        assert!(
            fr.is_empty() && out.is_empty(),
            "reduced motion must not resurrect a done cat: {fr:?} {out:?}"
        );
        assert_eq!(wd.drain_kitty_sightings().count(), 0, "no zombie re-log");
        let (fr, out, _) = tick_cat_at(&mut wd, at(6_100), &c, g, None, true);
        assert!(
            fr.is_empty() && out.is_empty(),
            "…and stays gone on refocus"
        );

        // Paw style draws no graphic, so the feline output is always empty.
        let mut c_paw = cfg();
        c_paw.feline_style = FelineStyle::Paw;
        let mut wd = WordDecorations::default();
        wd.rescan(&cat_term, 4, 20, &lex, &c_paw, 1, t0);
        tick_cat_at(&mut wd, t0, &c_paw, g, None, true);
        let (fr, out, _) = tick_cat_at(&mut wd, at(100), &c_paw, g, None, true);
        assert!(
            fr.is_empty() && !out.iter().any(|d| matches!(d.glyph, DecoGlyph::Paw)),
            "paw style draws no graphic under v4"
        );
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            0,
            "paw style logs no sighting"
        );
        let (_, out, _) = tick_cat_at(&mut wd, at(4_050), &reduce(&c_paw), g, None, true);
        assert!(out.is_empty(), "reduced motion resurrects nothing");

        // Nova ember: after the window and fade, reduce: no static glint.
        let c_nova = cfg_nova();
        let mut term = Terminal::new(2, 20);
        term.process(b"oh fuck");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 20, &lex, &c_nova, 1, t0);
        tick_nova(&mut wd, t0, &c_nova, g, None, None); // grant at t0
        let dur = u64::from(nova_features(wd.occ[0].genome.gkey).duration_ms);
        let (_, out, ..) = tick_nova(
            &mut wd,
            at(dur + RESIDUAL_FADE_MS + 100),
            &c_nova,
            g,
            None,
            None,
        );
        assert!(out.is_empty(), "the ember fade completed");
        let (q, out, ..) = tick_nova(
            &mut wd,
            at(dur + RESIDUAL_FADE_MS + 150),
            &reduce(&c_nova),
            g,
            None,
            None,
        );
        assert!(
            q.is_empty() && out.is_empty(),
            "no static-glint resurrection: {out:?}"
        );

        // Sparkle residual: anim window + fade complete, then reduce.
        let c_sp = cfg();
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 20, &lex, &c_sp, 1, t0);
        let mut out = Vec::new();
        tick_deco(&mut wd, t0, &c_sp, &mut out);
        tick_deco(
            &mut wd,
            at(c_sp.anim_ms + RESIDUAL_FADE_MS + 100),
            &c_sp,
            &mut out,
        );
        assert!(out.is_empty(), "the residual self-terminated");
        tick_deco(
            &mut wd,
            at(c_sp.anim_ms + RESIDUAL_FADE_MS + 150),
            &reduce(&c_sp),
            &mut out,
        );
        assert!(out.is_empty(), "no static-spark resurrection: {out:?}");
    }

    /// A landed peek records exactly one positioned cue; later frames none.
    #[test]
    fn a_peek_landing_records_one_positioned_cue() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        // Drive the whole rise and dwell, draining every frame like a host.
        let mut cues: Vec<PeekCue> = Vec::new();
        for k in 0..60u64 {
            let _ = tick_cat_at(
                &mut wd,
                t0 + Duration::from_millis(k * 16),
                &c,
                g,
                None,
                true,
            );
            cues.extend(wd.drain_peek_cues());
        }
        assert_eq!(cues.len(), 1, "once per episode, at the landing");
        let cue = cues[0];
        assert_eq!((cue.row, cue.col), (2, 8), "the word's viewport cell");
        let (x0, x1, y0, y1) = cue.head_px;
        assert!(x1 > x0 && y1 > y0, "a real dest rect: {:?}", cue.head_px);
        assert!(
            y0 >= 0 && y1 <= i32::from(g.rows) * i32::from(g.cell_h),
            "clamped to the viewport like the sprite it mirrors"
        );
    }

    /// A focused reduce-motion user gets the static pose at once, logged once,
    /// byte-stable. Un-reducing after that showing, past the peek window,
    /// marks the peek done: no entrance replay.
    #[test]
    fn reduced_from_birth_shows_a_static_pose_once_and_un_reducing_spends_it() {
        let lex = lex();
        let c = cfg();
        let mut c_red = cfg();
        c_red.reduced_motion = true;
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c_red, 1, t0);
        let (fr1, out1, fp1) = tick_cat_at(&mut wd, t0, &c_red, g, None, true);
        assert!(
            !fr1.is_empty() || !out1.is_empty(),
            "the static pose lands on the first focused tick"
        );
        assert_eq!(wd.drain_kitty_sightings().count(), 1, "logs once");
        let cat = wd
            .occ
            .iter()
            .find(|o| o.spec.graphic.is_some())
            .expect("the cat word scanned")
            .ident;
        assert!(
            wd.persist[&cat].phase_start.is_some(),
            "the phase clock is latched (at birth)"
        );
        // Far past the (never-running) peek window: the pose persists.
        let (fr2, out2, fp2) = tick_cat_at(&mut wd, at(60_000), &c_red, g, None, true);
        assert_eq!(fr1, fr2, "the pose persists (flags never advance)");
        assert_eq!(out1, out2);
        assert_eq!(fp1, fp2, "byte-stable static pose");
        assert_eq!(wd.drain_kitty_sightings().count(), 0, "exactly once");
        assert!(!wd.is_active(at(60_000)), "static = zero wakes");
        assert!(
            !wd.persist[&cat].peek_done,
            "flags hold while reduced (the pose persists)"
        );

        // Un-reduce: the first tick spends the one-shot (the static showing
        // was the showing).
        let (fr, _, _) = tick_cat_at(&mut wd, at(61_000), &c, g, None, true);
        assert!(
            fr.is_empty(),
            "zero free sprites — no entrance replay on un-reduce: {fr:?}"
        );
        assert!(
            wd.persist[&cat].peek_done,
            "the elapsed static window latches peek_done"
        );
        assert!(!wd.is_active(at(61_000)), "done = zero wakes");
        let (fr, _, _) = tick_cat_at(&mut wd, at(62_000), &c, g, None, true);
        assert!(fr.is_empty(), "the peek stays spent (done forever)");
    }

    /// A capture during a freeze neither rescans nor ticks: episodes survive
    /// byte for byte and resume after thaw.
    #[test]
    fn capture_during_freeze_preserves_episodes_byte_for_byte() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        tick_cat_at(&mut wd, t0, &c, g, None, true); // first present (the rise runs from birth)
        let snap = |wd: &WordDecorations| {
            let mut v: Vec<_> = wd
                .persist
                .iter()
                .map(|(id, e)| {
                    (
                        *id,
                        e.appeared,
                        e.phase_start,
                        e.genome,
                        e.peek_started,
                        e.peek_done,
                        e.sweep_started,
                        e.sweep_done,
                        e.logged,
                        e.burst_roll,
                    )
                })
                .collect();
            v.sort_by_key(|s| s.0);
            v
        };
        let before = snap(&wd);
        assert!(!before.is_empty(), "the fixture built episodes");
        // The marks before the freeze; the frozen rescan must add none.
        let marks_before: std::collections::BTreeSet<u64> =
            wd.done_marks.index.keys().copied().collect();
        wd.freeze(at(100));
        // A capture rescan 20 s into the freeze, on a blank grid: a no-op.
        let mut blank = Terminal::new(4, 20);
        wd.rescan(&blank, 4, 20, &lex, &c, 2, at(20_000));
        assert!(wd.needs_rescan(2), "the pending rescan stays pending");
        assert_eq!(snap(&wd), before, "episodes byte-for-byte (term rescan)");
        let mut snap_input = aterm_core::render::RenderInput::default();
        blank.cell_frame_into(&mut snap_input, 4, 20);
        wd.rescan_from_cells(
            &snap_input.cells,
            &snap_input.line_sizes,
            4,
            20,
            &lex,
            &c,
            2,
            at(20_000),
        );
        assert!(wd.needs_rescan(2), "the snapshot entry point no-ops too");
        assert_eq!(snap(&wd), before, "episodes byte-for-byte (cells rescan)");
        assert_eq!(
            wd.done_marks
                .index
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            marks_before,
            "no grace-expiry done marks — the frozen rescan added nothing"
        );
        // The frozen tick emits nothing and advances nothing (defensive).
        let (fr, out, fp) = tick_cat_at(&mut wd, at(20_000), &c, g, None, true);
        assert!(
            fr.is_empty() && out.is_empty() && fp == 0,
            "frozen tick no-ops"
        );
        assert_eq!(snap(&wd), before);
        // Thaw and rescan: the peek resumes ~100 ms in.
        wd.thaw(at(20_000));
        wd.rescan(&term, 4, 20, &lex, &c, 2, at(20_000));
        assert!(!wd.needs_rescan(2));
        let (fr, _, _) = tick_cat_at(&mut wd, at(20_050), &c, g, None, true);
        assert!(!fr.is_empty(), "the one-shot resumed mid-rise after thaw");
        assert!(wd.is_active(at(20_050)));
        let cat = wd
            .occ
            .iter()
            .find(|o| o.spec.graphic.is_some())
            .expect("the cat word rescanned");
        assert!(!wd.persist[&cat.ident].peek_done && !cat.inert);
    }

    /// Two-way burst mutex: a live classic nova defers the supernova grant
    /// until its window ends.
    #[test]
    fn live_classic_nova_defers_the_supernova_grant() {
        let (table, lex_frag) = custom_table(
            "[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n",
        );
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("override parses");
        let mut c = cfg_rainbow(100);
        c.spec_table = table;
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let g = EffectGeom {
            rows: 8,
            ..geom20()
        };
        let mut term = Terminal::new(8, 20);
        term.process(b"boom");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        let boom = wd.occ[0].ident;
        assert_eq!(wd.persist[&boom].nova_start, Some(t0), "classic nova live");
        let dur = u64::from(nova_features(wd.occ[0].genome.gkey).duration_ms);
        // The profanity appears mid-window: its rolled supernova defers.
        term.process(b"\x1b[6;1Hoh fuck");
        wd.rescan(&term, 8, 20, &lexicon, &c, 2, at(100));
        let fuck = wd
            .occ
            .iter()
            .find(|o| o.spec.burst.map(|b| b.kind) == Some(BurstKind::SuperNova))
            .expect("the supernova word scanned")
            .ident;
        assert!(wd.persist[&fuck].burst_roll, "chance = 100 rolled");
        tick_nova(&mut wd, at(100), &c, g, None, None);
        assert!(
            wd.persist[&fuck].nova_start.is_none(),
            "deferred behind the live classic (mid-window)"
        );
        tick_nova(&mut wd, at(dur - 50), &c, g, None, None);
        assert!(
            wd.persist[&fuck].nova_start.is_none(),
            "still deferred at the classic window's tail"
        );
        // Past the classic window: the mutex releases and the grant lands.
        tick_nova(&mut wd, at(dur + 100), &c, g, None, None);
        assert!(
            wd.persist[&fuck].nova_start.is_some(),
            "granted once the classic window ended"
        );
    }

    /// The eclipse veil and every other Over-blend supernova deco drop over
    /// selected cells (the renderer freezes only the Add stream).
    #[test]
    fn selection_mid_eclipse_punches_through_the_veil() {
        let lex = lex();
        let c = cfg_rainbow(100);
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 8,
            cols: 24,
        };
        let t0 = Instant::now();
        let mut term = Terminal::new(8, 24);
        // White cell backgrounds ⇒ the eclipse branch.
        term.process(b"\x1b[48;2;255;255;255m\r\n\r\n\r\nfuck\x1b[0m");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 8, 24, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None); // grant (no selection yet)
        let sel = select_span(1, 8, 11);
        let sv = SelView {
            sel: &sel,
            display_offset: 0,
        };
        let (_, out, ..) = tick_nova(
            &mut wd,
            t0 + Duration::from_millis(500),
            &c,
            g,
            None,
            Some(sv),
        );
        let shade: Vec<&WordDecoration> = out
            .iter()
            .filter(|d| matches!(d.glyph, DecoGlyph::Shade))
            .collect();
        assert!(!shade.is_empty(), "mid-detonation veil present");
        assert!(
            !shade
                .iter()
                .any(|d| d.row == 1 && (8..=11).contains(&d.col)),
            "no Shade stamp lands on a selected cell"
        );
        assert!(
            shade.iter().any(|d| d.row == 1),
            "the selected ROW keeps its unselected veil cells (punch-through, not row deletion)"
        );
        // A debris-phase frame (Over-blend motes, no Shade): the filter keys on
        // the blend, so a selected mote cell drops.
        let td = t0 + Duration::from_millis(1800);
        let (_, out_free, ..) = tick_nova(&mut wd, td, &c, g, None, None);
        let mote = out_free
            .iter()
            .find(|d| matches!(d.blend, DecoBlend::Over) && !matches!(d.glyph, DecoGlyph::Shade))
            .copied()
            .expect("light-theme debris motes ride the Over deco stream");
        let sel_d = select_span(i32::from(mote.row), mote.col, mote.col);
        let sv_d = SelView {
            sel: &sel_d,
            display_offset: 0,
        };
        let (_, out_sel, ..) = tick_nova(&mut wd, td, &c, g, None, Some(sv_d));
        assert!(
            !out_sel
                .iter()
                .any(|d| matches!(d.blend, DecoBlend::Over) && sv_d.cell_selected(d.row, d.col)),
            "no Over-blend debris mote renders over a selected cell"
        );
        assert!(
            out_sel.iter().any(|d| matches!(d.blend, DecoBlend::Over)),
            "unselected debris survives the filter (punch-through, not deletion)"
        );
    }

    /// `chance_pct` gates every burst kind: a starburst at 0 never fires, at
    /// 100 always does.
    #[test]
    fn burst_chance_pct_gates_starburst_rolls() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(2, 20);
        term.process(b"go zap");
        let mut out = Vec::new();
        // chance = 0: never fires.
        let (c0, lex0) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = { kind = \"starburst\", chance = 0 }\n",
        );
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 20, &lex0, &c0, 1, t0);
        assert!(
            !wd.persist[&wd.occ[0].ident].burst_roll,
            "chance = 0 never rolls"
        );
        tick_deco(&mut wd, at(50), &c0, &mut out);
        assert!(out.is_empty(), "chance = 0 starburst never fires: {out:?}");
        assert!(!wd.is_active(at(50)), "…and never arms");
        let mut c0r = c0.clone();
        c0r.reduced_motion = true;
        tick_deco(&mut wd, at(100), &c0r, &mut out);
        assert!(
            out.is_empty(),
            "no reduced-motion static spark for a rolled-off burst"
        );
        // chance = 100: always fires.
        let (c100, lex100) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = { kind = \"starburst\", chance = 100 }\n",
        );
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 20, &lex100, &c100, 1, t0);
        assert!(
            wd.persist[&wd.occ[0].ident].burst_roll,
            "chance = 100 always rolls"
        );
        tick_deco(&mut wd, at(50), &c100, &mut out);
        assert!(!out.is_empty(), "chance = 100 always fires");
    }

    /// With ink disabled, a light-theme supernova still takes the eclipse
    /// branch (the lead-cell bg is captured for every occurrence).
    #[test]
    fn ink_disabled_light_bg_still_takes_the_eclipse_branch() {
        let lex = lex();
        let mut c = cfg_rainbow(100);
        c.ink_enabled = false;
        let t0 = Instant::now();
        let g = geom20();
        let mut term = Terminal::new(2, 20);
        term.process(b"\x1b[48;2;255;255;255moh fuck\x1b[0m");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        let o = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Profanity)
            .expect("the word scanned");
        assert_eq!(o.ink_cells, 0, "ink disabled: nothing captured for ink");
        assert_eq!(
            o.ink_bg,
            [255, 255, 255],
            "…but the lead-cell bg is still captured for the theme branch"
        );
        tick_nova(&mut wd, t0, &c, g, None, None); // grant at t0
        // Mid-detonation: the light-theme eclipse, not the dark wash.
        let (q, out, ..) = tick_nova(&mut wd, t0 + Duration::from_millis(500), &c, g, None, None);
        let veil = out
            .iter()
            .filter(|d| matches!(d.blend, DecoBlend::Over))
            .count();
        assert!(veil > 50, "the Over-blend dark veil is present, got {veil}");
        let wash = q
            .iter()
            .filter(|x| i32::from(x.w) > 8 * i32::from(g.cell_w))
            .count();
        assert_eq!(wash, 0, "no full-viewport additive wash on a light bg");
    }

    /// §3.1: the rainbow guard samples four points, so a word whose mid sample
    /// passes while an endpoint fails is still pulled to [`MIN_INK_CONTRAST`].
    #[test]
    fn rainbow_guard_samples_four_points_not_just_mid() {
        let t0 = Instant::now();
        let c = cfg_rainbow(0);
        let bg = [0x80u8, 0x80, 0x80]; // mid gray: dark branch, washout-prone
        let fg = [255u8, 255, 255]; // pulling toward white restores contrast
        let strength = c.ink_strength;
        let bg32 = rgb3_to_u32(bg);
        let sample = |gkey: u64, u: f32| {
            let h = hsv2rgb(
                (rainbow_base_hue(gkey) + u * 300.0).rem_euclid(360.0),
                0.85,
                1.0,
            );
            contrast_ratio(mix_rgb(rgb3_to_u32(fg), h, strength), bg32)
        };
        // A gkey whose mid sample passes at full strength while another fails.
        let gkey = (0..10_000u64)
            .find(|g| {
                sample(*g, 0.5) >= MIN_INK_CONTRAST
                    && [0.0f32, 1.0 / 3.0, 2.0 / 3.0, 1.0]
                        .iter()
                        .any(|u| sample(*g, *u) < MIN_INK_CONTRAST)
            })
            .expect("a mid-blind gkey exists");
        let mut o = occ(Class::Profanity, t0);
        o.spec = class_default_spec(Class::Profanity, &c);
        o.genome = Genome { gkey, magic: 100 };
        o.start_col = 2;
        o.end_col = 5;
        o.ink_cells = 4;
        o.ink_bg = bg;
        let mut wd = WordDecorations {
            occ: vec![o],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        wd.ink_base_fg = vec![fg; 4];
        wd.ink_cols = (2..6).collect();
        let settle = t0 + Duration::from_millis(2_600);
        let (_, _, ink, _, _) = tick_nova(&mut wd, settle, &c, geom20(), None, None);
        assert_eq!(ink.len(), 4);
        // The 4 lead cells sit at the guard's sample points; each clears.
        for cell in &ink {
            assert!(
                contrast_ratio(rgb3_to_u32(cell.color), bg32) >= MIN_INK_CONTRAST,
                "guarded cell washes out: {cell:?}"
            );
        }
        // The guard pulled: the failing sample's raw bytes were not emitted.
        let denom = 3.0f32;
        let raw: Vec<[u8; 3]> = (0..4u16)
            .map(|i| {
                let u = f32::from(i) / denom;
                u32_to_rgb3(mix_rgb(
                    rgb3_to_u32(fg),
                    hsv2rgb(
                        (rainbow_base_hue(gkey) + u * 300.0).rem_euclid(360.0),
                        0.85,
                        1.0,
                    ),
                    strength,
                ))
            })
            .collect();
        assert_ne!(
            ink.iter().map(|i| i.color).collect::<Vec<_>>(),
            raw,
            "the guard pulled strength below the raw full-strength mix"
        );
    }

    /// [`WordDecorations::kitty_cursor`] emits exactly its body sprite, just
    /// ahead of the cursor cell and centred on its row.
    #[test]
    fn kitty_emits_only_its_body_in_front_of_the_cursor() {
        let g = geom20(); // 10×20 cells, 6×20 grid.
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        let footprint = wd
            .kitty_cursor_footprint(still_layout(g, (3, 5)))
            .expect("visible footprint");

        wd.kitty_cursor(
            KittyCursorFrame {
                alpha: 200,
                ..still_frame(g, (3, 5))
            },
            &mut free,
        );
        assert_eq!(free.len(), 1, "cursor kitty emits exactly its body");
        let s = free[0];
        assert_eq!(
            footprint,
            CatFootprint {
                x: s.x,
                y: s.y,
                w: s.w,
                h: s.h,
            },
            "palette footprint and emitted sprite geometry must be identical"
        );
        assert!(matches!(s.z, FreeZ::OverText));
        assert!(matches!(s.sampler, FreeSampler::Nearest));
        assert_eq!(s.alpha, 200);
        assert_eq!(s.aw, s.w, "NEAREST 1:1 width");
        assert_eq!(s.ah, s.h, "NEAREST 1:1 height");
        assert!(s.w > 0 && s.h > 0);
        // The sprite's left edge is past the cursor cell's right edge (x=60),
        // within a cell of it.
        let cursor_right = 6 * i32::from(g.cell_w);
        assert!(
            s.x > cursor_right && s.x < cursor_right + i32::from(g.cell_w),
            "the companion leads the cursor cell by less than a cell: x={}",
            s.x
        );
        // Vertically centred on row 3 (centre y = 3*20 + 10 = 70).
        let mid = s.y + i32::from(s.h) / 2;
        assert!(
            (mid - 70).abs() <= 1,
            "centred on the cursor row, got mid={mid}"
        );
        assert!(wd.free_atlas().is_some(), "baked into the shared atlas");

        // alpha 0 ⇒ nothing.
        let mut none = Vec::new();
        wd.kitty_cursor(
            KittyCursorFrame {
                alpha: 0,
                ..still_frame(g, (3, 5))
            },
            &mut none,
        );
        assert!(none.is_empty());
    }

    /// With a sing drive, ♪/♫ sprites emit beside the body, capped at
    /// `MAX_NOTES`, rainbow-tinted over white tiles, alpha-scaled by the drive.
    #[test]
    fn singing_notes_stream_ring_capped_tinted_and_drive_scaled() {
        use crate::kitty_sing::{MAX_NOTES, NOTE_TINTS, NoteKind, NoteSprite};
        let g = geom20();
        let mut wd = WordDecorations::default();
        wd.set_kitty_sprite_source(KittySpriteSource::Custom {
            source_fp: 1,
            w: 4,
            h: 4,
            rgba: Arc::from(vec![255u8; 4 * 4 * 4]),
        });
        let mut notes = [None; MAX_NOTES];
        for (i, slot) in notes.iter_mut().enumerate() {
            *slot = Some(NoteSprite {
                dx: 0.2 + i as f32 * 0.03,
                dy: -0.2 - i as f32 * 0.05,
                alpha: 200,
                kind: NoteKind::Eighth, // one kind ⇒ one bake serves all 16
                tint: NOTE_TINTS[i % NOTE_TINTS.len()],
            });
        }
        let frame = |sing: f32, notes| KittyCursorFrame {
            sing,
            notes,
            ..still_frame(g, (3, 2))
        };
        let mut free = Vec::new();
        wd.kitty_cursor(frame(1.0, notes), &mut free);
        let body = *free.last().expect("the body emits last (topmost)");
        let emitted = &free[..free.len() - 1];
        assert_eq!(
            emitted.len(),
            MAX_NOTES,
            "a full ring emits exactly the cap, never more"
        );
        assert!(
            emitted.iter().all(|s| s.tint != 0x00FF_FFFF),
            "notes carry their rainbow tint (the body stays neutral)"
        );
        assert!(
            emitted.iter().all(|s| s.alpha == 200),
            "full drive passes the note envelope through"
        );
        assert!(
            emitted.iter().all(|s| s.y < body.y + i32::from(body.h)),
            "notes rise from the mouth, never below the cat"
        );

        // Wind-down: the same notes at half drive emit at half alpha.
        let mut fading = Vec::new();
        wd.kitty_cursor(frame(0.5, notes), &mut fading);
        let faded = &fading[..fading.len() - 1];
        assert_eq!(faded.len(), MAX_NOTES);
        assert!(
            faded.iter().all(|s| s.alpha == 100),
            "the crossfade rides the emitted alpha"
        );

        // Zero drive (with a drained ring): byte-identical to a plain frame.
        let mut plain = Vec::new();
        wd.kitty_cursor(frame(0.0, [None; MAX_NOTES]), &mut plain);
        assert_eq!(plain.len(), 1, "no note work at rest — just the body");
    }

    /// The escort's drawn height stays within the baker's `2·cell_h` slot at
    /// every age ([`WordDecorations::COMPANION_CELL_H`]), so a size bump can
    /// never clamp the age axis flat.
    #[test]
    fn the_cursor_escort_fits_its_slot_at_every_age() {
        let g = geom20(); // cell_h = 20 ⇒ the slot is 40 px.
        let mut wd = WordDecorations::default();
        let emit = |wd: &mut WordDecorations, age| {
            // One emission per frame: each age is its own tile, and the atlas
            // admits two bakes per frame.
            wd.cat_baker_ready = false;
            let mut sprites = Vec::new();
            wd.kitty_cursor(
                KittyCursorFrame {
                    look: KittyLook {
                        age,
                        ..KittyLook::default()
                    },
                    ..still_frame(g, (2, 4))
                },
                &mut sprites,
            );
            assert_eq!(sprites.len(), 1, "the escort drew");
            sprites[0].h
        };
        // Every age fits the atlas slot — nothing is being clamped.
        for age in [
            CatAge::Kitten,
            CatAge::Adolescent,
            CatAge::Adult,
            CatAge::Elder,
        ] {
            let h = emit(&mut wd, age);
            assert!(
                i32::from(h) <= 2 * i32::from(g.cell_h),
                "{age:?} escort ({h} px) must fit the 2-cell bake slot"
            );
        }
        // …and the age axis is still an axis: the extremes differ.
        assert_ne!(
            emit(&mut wd, CatAge::Kitten),
            emit(&mut wd, CatAge::Elder),
            "a clamped-flat age axis would defeat the ceiling above"
        );
    }

    // ───────── one cat per animal word, centred on the word ─────────

    /// One typed animal word grows exactly one cat, its authored centre on the
    /// span midpoint for an odd (`cat`) and an even (`cats`) word.
    #[test]
    fn one_typed_animal_word_grows_exactly_one_cat_centred_on_its_span() {
        let lex = lex();
        let c = cfg();
        let mut still = cfg();
        still.reduced_motion = true;
        let g = geom20();
        let (rows, cols) = (g.rows as usize, g.cols as usize);
        let cw = i32::from(g.cell_w);
        let at = Instant::now() + Duration::from_millis(DWELL_QUIET_MS);

        // One cat, centred on the span, clear of both viewport edges (a clamped
        // head could pass the centre check by accident).
        let measure = |word: &str, cfg: &DecoConfig, slack: i32| {
            let t0 = Instant::now();
            let mut term = Terminal::new(g.rows, g.cols);
            term.process(format!("the {word} sat").as_bytes());
            let mut wd = WordDecorations::default();
            wd.rescan(&term, rows, cols, &lex, cfg, 1, t0);
            let occ = feline(&wd).clone();
            assert_eq!(
                (occ.start_col, occ.end_col),
                (4, 4 + word.chars().count() as u16 - 1),
                "PRECONDITION: {word:?} sits where the fixture typed it"
            );

            let (free, _, _) = tick_cat(&mut wd, at, cfg, g);
            assert_eq!(
                free.len(),
                1,
                "{word:?}: one animal word, ONE cat — got {free:?}"
            );
            let cat = free[0];
            assert!(
                cat.x > 0 && cat.x + i32::from(cat.w) < i32::from(g.cols) * cw,
                "PRECONDITION: {word:?}'s head clears both viewport edges, so no \
                 clamp is standing in for the centring law: {cat:?}"
            );

            // The span midpoint from first principles, not `cat_geometry_for`.
            let mid = (i32::from(occ.start_col) + i32::from(occ.end_col) + 1) * cw / 2;
            let variant = special_variant_v4(occ.genome.magic)
                .unwrap_or_else(|| cat_variant_v4(occ.genome.gkey));
            let center = f32::from(GLYPHS[variant as usize].center_x) / f32::from(FIXED_ONE);
            let drawn_center = cat.x + (center * f32::from(cat.w)).round() as i32;
            assert!(
                (drawn_center - mid).abs() <= slack,
                "{word:?}: the head's authored centre must land on the word's \
                 midpoint (span {}..={} ⇒ mid {mid} px, drawn {drawn_center} px, \
                 slack {slack}); {cat:?}",
                occ.start_col,
                occ.end_col
            );
        };

        for word in ["cat", "cats"] {
            // Reduced motion: the settled pose, so the anchor is byte-exact.
            measure(word, &still, 0);
            // Full motion: the idle sway is under 1 px at this cell.
            measure(word, &c, 1);
        }
    }

    /// The custom sprite's footprint matches emission, clamping at the right
    /// edge and showing the lower part of the body on row zero.
    /// The 4×2 source is nearest-resampled to fit the slot, aspect kept
    /// (`min(80/4, 36/2) = 18` ⇒ 72×36), and clearing it restores the
    /// built-in homage at its own size.
    #[test]
    fn custom_kitty_footprint_matches_emission_at_interior_right_and_top_edges() {
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let mut wd = WordDecorations::default();
        wd.set_kitty_sprite_source(KittySpriteSource::Custom {
            source_fp: 0x101,
            w: 4,
            h: 2,
            rgba: Arc::from(vec![0xff; 4 * 2 * 4]),
        });
        let emit = |wd: &mut WordDecorations, cursor: (u16, u16)| {
            let expected = wd
                .kitty_cursor_footprint(still_layout(g, cursor))
                .expect("custom sprite remains placeable");
            let mut free = Vec::new();
            wd.kitty_cursor(still_frame(g, cursor), &mut free)
                .expect("custom sprite emits");
            assert_eq!(free.len(), 1, "custom art is exactly one body");
            let body = free[0];
            assert_eq!(
                expected,
                CatFootprint {
                    x: body.x,
                    y: body.y,
                    w: body.w,
                    h: body.h,
                },
                "palette/custody geometry is the body actually sent to the renderer"
            );
            body
        };

        let interior = emit(&mut wd, (3, 5));
        assert_eq!((interior.w, interior.h), (72, 36));
        assert!(interior.x >= 0 && interior.x + i32::from(interior.w) <= grid_w);

        let right = emit(&mut wd, (3, g.cols - 1));
        assert_eq!(
            right.x + i32::from(right.w),
            grid_w,
            "the configured body clamps exactly at the right edge"
        );
        assert!(right.x >= 0);

        let top_right = emit(&mut wd, (0, g.cols - 1));
        assert_eq!(top_right.x + i32::from(top_right.w), grid_w);
        assert!(
            top_right.y < 0 && top_right.y + i32::from(top_right.h) > 0,
            "row-zero custom art is partially clipped, never wholly misplaced or dropped: \
             {top_right:?}"
        );

        wd.set_kitty_sprite_source(KittySpriteSource::BuiltIn);
        let homage = emit(&mut wd, (3, 5));
        assert_ne!(
            (homage.w, homage.h),
            (interior.w, interior.h),
            "clearing the custom art restores the built-in homage"
        );
    }

    /// A same-size asset replacement rebases a fold and repaints; an invalid
    /// one fails closed, keeping nothing of the last valid generation.
    #[test]
    fn custom_kitty_hot_reload_rebases_repaints_and_invalid_replacement_is_dark() {
        use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

        let g = geom20();
        let t0 = Instant::now();
        let red: Arc<[u8]> = Arc::from([0xff, 0x10, 0x20, 0xff].repeat(8));
        let green: Arc<[u8]> = Arc::from([0x20, 0xee, 0x60, 0xff].repeat(8));
        let mut wd = WordDecorations::default();
        wd.set_kitty_sprite_source(KittySpriteSource::Custom {
            source_fp: 0x201,
            w: 4,
            h: 2,
            rgba: Arc::clone(&red),
        });

        let layout = |cursor| still_layout(g, cursor);
        let emit =
            |wd: &mut WordDecorations, cursor: (u16, u16), motion: CursorCatPlacementFrame| {
                let placement = wd
                    .resolve_kitty_cursor_placement(layout(cursor), motion)
                    .expect("custom kitty placement");
                let mut free = Vec::new();
                let fp = wd
                    .kitty_cursor_at_placement(still_frame(g, cursor), placement, &mut free)
                    .expect("custom kitty emission");
                (placement, free[0], fp)
            };
        let atlas_pixel = |atlas: &aterm_render::SceneAtlas, sprite: FreeSprite| {
            let offset =
                (usize::from(sprite.ay) * atlas.width as usize + usize::from(sprite.ax)) * 4;
            <[u8; 4]>::try_from(&atlas.rgba[offset..offset + 4]).expect("atlas texel")
        };

        let old_cursor = (3, g.cols - 1);
        let (_, red_body, red_fp) = emit(&mut wd, old_cursor, settled_at(t0));
        let red_atlas = wd.free_atlas().expect("red atlas");
        assert_eq!(atlas_pixel(&red_atlas, red_body), [0xff, 0x10, 0x20, 0xff]);

        let started = t0 + Duration::from_millis(16);
        let folding = |progress| CursorCatPlacementFrame {
            sampled_at: started + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started,
                direction: CatFoldDirection::Forward,
                progress,
            }),
            facing_left: false,
        };
        let (moving, _, _) = emit(&mut wd, (4, 0), folding(0.2));
        assert!(moving.in_motion(), "negative control: old art is mid-fold");

        wd.set_kitty_sprite_source(KittySpriteSource::Custom {
            source_fp: 0x202,
            w: 4,
            h: 2,
            rgba: Arc::clone(&green),
        });
        assert_eq!(wd.kitty_sprite_source_fingerprint(), Some(0x202));
        assert!(Arc::ptr_eq(
            wd.kitty_sprite_rgba().expect("replacement source"),
            &green
        ));
        let (replaced, green_body, green_fp) = emit(&mut wd, (4, 0), folding(0.3));
        assert!(
            !replaced.in_motion(),
            "the replacement cannot inherit the old asset's off-glass fold"
        );
        assert_eq!(
            (green_body.x, green_body.y, green_body.w, green_body.h),
            (
                replaced.sample.x,
                replaced.sample.y,
                replaced.sample.w,
                replaced.sample.h
            )
        );
        let green_atlas = wd.free_atlas().expect("replacement atlas");
        assert_eq!(
            atlas_pixel(&green_atlas, green_body),
            [0x20, 0xee, 0x60, 0xff],
            "same-size hot reload addresses replacement texels"
        );
        assert!(green_atlas.version > red_atlas.version);
        assert_ne!(green_fp, red_fp, "the repaint key moves with the new art");

        wd.set_kitty_sprite_source(KittySpriteSource::Custom {
            source_fp: 0x203,
            w: 4,
            h: 2,
            rgba: Arc::from(vec![0u8; 4 * 2 * 4 - 1]),
        });
        assert_eq!(wd.kitty_sprite_source_fingerprint(), None);
        assert!(!wd.has_custom_kitty_sprite());
        assert!(wd.kitty_sprite_rgba().is_none());
        assert!(wd.kitty_cursor_footprint(layout((4, 0))).is_none());
        let mut stale = Vec::new();
        assert!(
            wd.kitty_cursor(still_frame(g, (4, 0)), &mut stale,)
                .is_none()
        );
        assert!(
            stale.is_empty(),
            "invalid reload cannot retain the old body"
        );
    }

    /// Body stretch and forward lead clamp together at the right margin.
    #[test]
    fn cursor_cat_body_clamps_after_right_shift_stretch_and_lead() {
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        wd.kitty_cursor(
            KittyCursorFrame {
                pose: crate::kitty_cursor::CatPose {
                    scale_x: 1.17,
                    lead: 0.22,
                    ..crate::kitty_cursor::CatPose::STILL
                },
                // Last column: the body clamps to the edge.
                ..still_frame(g, (3, g.cols - 1))
            },
            &mut free,
        );
        assert_eq!(free.len(), 1, "the cursor kitty emits only its body");
        let body = free[0];
        assert_eq!(
            body.x + i32::from(body.w),
            grid_w,
            "shift, stretch, and pose lead must still clamp to the grid edge"
        );
        assert!(body.x >= 0, "body never crosses the left edge");
    }

    /// A forward line fold launches from the body actually drawn and lands
    /// without a snap; the palette footprint stays at the live caret. (The
    /// off-glass leave and cross legs are the conformance walk's
    /// `cursor_cat_fold_conformance_real_placement_and_teleport_mutant`.)
    #[test]
    fn cursor_cat_fold_launches_from_the_drawn_body_and_lands_without_a_snap() {
        use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion);

        let old_cursor = (3, g.cols - 1);
        let (_, old) = emit(&mut wd, old_cursor, settled_at(t0));
        assert_eq!(old.x + i32::from(old.w), grid_w, "seed at right edge");

        let started = t0 + Duration::from_millis(16);
        let motion = |progress: f32, sampled_at: Instant| CursorCatPlacementFrame {
            sampled_at,
            fold: Some(CatFoldFrame {
                started,
                direction: CatFoldDirection::Forward,
                progress,
            }),
            facing_left: false,
        };
        let new_cursor = (4, 0);
        let (launch_place, launch) = emit(&mut wd, new_cursor, motion(0.0, started));
        assert_eq!(
            (launch.x, launch.y),
            (old.x, old.y),
            "the first fold sample is C0-continuous with the body actually drawn"
        );
        assert_eq!(
            launch_place.sample,
            wd.kitty_cursor_footprint(still_layout(g, new_cursor))
                .expect("logical target"),
            "palette sampling follows the live target, never the offstage body"
        );

        // The target moves during entry; the landing glides onto exactly the
        // direct emitter's bytes, no snap.
        let advanced = (4, 3);
        let (entry_place, entry_body) = emit(
            &mut wd,
            advanced,
            motion(0.85, started + Duration::from_millis(238)),
        );
        assert_eq!(entry_place.sample.x, 4 * i32::from(g.cell_w) + 7);
        let mut at = started + crate::kitty_cursor::CURSOR_FOLD_DURATION;
        let (_, mut landed) = emit(&mut wd, advanced, settled_at(at));
        assert!(
            (landed.x - entry_body.x).abs() < 2 * i32::from(g.cell_w),
            "the landing glides from the entry body, no end snap: {entry_body:?} -> {landed:?}"
        );
        for _ in 0..40 {
            at += Duration::from_millis(16);
            let (_, body) = emit(&mut wd, advanced, settled_at(at));
            landed = body;
        }
        let mut direct = Vec::new();
        wd.kitty_cursor(still_frame(g, advanced), &mut direct);
        assert_eq!(
            landed, direct[0],
            "the seam converges on the exact settled frame"
        );
    }

    #[test]
    fn reverse_fold_mirrors_reenters_and_rebaselines_on_coordinate_breaks() {
        use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();

        let emit =
            |wd: &mut WordDecorations, geom, cursor, motion| emit_placed(wd, geom, cursor, motion);
        let _ = emit(&mut wd, g, (3, 0), settled_at(t0));
        let started = t0 + Duration::from_millis(16);
        let fold = |progress| CursorCatPlacementFrame {
            sampled_at: started + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started,
                direction: CatFoldDirection::Reverse,
                progress,
            }),
            facing_left: true,
        };
        let (_, left_off) = emit(&mut wd, g, (2, g.cols - 1), fold(0.499));
        assert!(left_off.flip_x);
        assert!(left_off.x + i32::from(left_off.w) <= 0);
        let (_, right_off) = emit(&mut wd, g, (2, g.cols - 1), fold(0.5));
        assert!(right_off.flip_x);
        assert!(right_off.x >= grid_w);

        // A second fold before the first lands starts from the body emitted
        // last frame.
        let reentered_at = started + Duration::from_millis(150);
        let reentered = |progress| CursorCatPlacementFrame {
            sampled_at: reentered_at + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started: reentered_at,
                direction: CatFoldDirection::Forward,
                progress,
            }),
            facing_left: false,
        };
        let (_, restarted) = emit(&mut wd, g, (3, 0), reentered(0.0));
        assert_eq!(
            (restarted.x, restarted.y),
            (right_off.x, right_off.y),
            "a reentrant fold is C0-continuous with the last emitted body"
        );

        // A missing target retires the coordinate space; the same fold stamp
        // cannot restart.
        let zero_width = EffectGeom { cell_w: 0, ..g };
        assert!(
            wd.resolve_kitty_cursor_placement(still_layout(zero_width, (3, 0)), reentered(0.1))
                .is_none()
        );
        let (reappeared, body) = emit(&mut wd, g, (3, 0), reentered(0.2));
        assert!(!reappeared.in_motion());
        assert!(body.x >= 0 && body.x + i32::from(body.w) <= grid_w);

        // A source replacement resets even at the same built-in dimensions.
        wd.set_kitty_sprite_source(KittySpriteSource::BuiltIn);
        let (replaced, _) = emit(&mut wd, g, (3, 0), reentered(0.3));
        assert!(!replaced.in_motion());

        let resized = EffectGeom {
            cols: g.cols + 1,
            ..g
        };
        let (place, body) = emit(&mut wd, resized, (3, 0), reentered(0.4));
        assert!(
            !place.in_motion(),
            "geometry changes settle instead of interpolating incomparable spaces"
        );
        assert!(body.x >= 0 && body.x + i32::from(body.w) <= 210);

        // An explicit rebase settles the live fold token rather than replaying
        // it.
        let torn_at = reentered_at + Duration::from_millis(160);
        let torn_fold = |progress| CursorCatPlacementFrame {
            sampled_at: torn_at + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started: torn_at,
                direction: CatFoldDirection::Forward,
                progress,
            }),
            facing_left: false,
        };
        let (moving, _) = emit(&mut wd, resized, (3, 0), torn_fold(0.1));
        assert!(moving.in_motion());
        wd.rebase_kitty_cursor_placement();
        let (rebased, body) = emit(&mut wd, resized, (3, 0), torn_fold(0.2));
        assert!(!rebased.in_motion());
        assert!(body.x >= 0 && body.x + i32::from(body.w) <= 210);

        // The session is in the placement key: a tab switch rebaselines.
        let session_fold_at = torn_at + Duration::from_millis(80);
        let session_fold = |progress| CursorCatPlacementFrame {
            sampled_at: session_fold_at + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started: session_fold_at,
                direction: CatFoldDirection::Reverse,
                progress,
            }),
            facing_left: true,
        };
        let (moving, _) = emit(&mut wd, resized, (2, g.cols), session_fold(0.1));
        assert!(
            moving.in_motion(),
            "negative control: a fresh fold animates"
        );
        wd.set_scan_session(Some(0xCA7));
        let (switched, _) = emit(&mut wd, resized, (2, g.cols), session_fold(0.2));
        assert!(!switched.in_motion(), "a new session settles old travel");
    }

    #[test]
    fn sparkle_word_reset_preserves_cursor_companion_placement_and_atlas() {
        let g = geom20();
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        wd.begin_host_frame();
        let layout = still_layout(g, (3, 7));
        let placement = wd
            .resolve_kitty_cursor_placement(layout, settled_at(now))
            .expect("placeable companion");
        let mut free = Vec::new();
        wd.kitty_cursor_at_placement(still_frame(g, layout.cursor), placement, &mut free)
            .expect("companion emits");
        let atlas_before = wd.free_atlas().expect("companion baked an atlas");
        let key_before = wd.kitty_cursor_placement.key;
        let target_before = wd.kitty_cursor_placement.last_target;
        wd.have_scanned = true;
        wd.active_until = Some(now + Duration::from_secs(1));

        wd.hard_reset_words();

        assert!(!wd.have_scanned, "word scan state retires immediately");
        assert!(
            wd.active_until.is_none(),
            "word cadence retires immediately"
        );
        assert_eq!(wd.kitty_cursor_placement.key, key_before);
        assert_eq!(wd.kitty_cursor_placement.last_target, target_before);
        let atlas_after = wd.free_atlas().expect("companion atlas stays resident");
        assert!(
            Arc::ptr_eq(&atlas_before, &atlas_after),
            "a word-only toggle must not rebake the companion atlas"
        );

        wd.hard_reset();
        assert!(
            wd.kitty_cursor_placement.key.is_none(),
            "negative control: a full lifecycle reset retires placement"
        );
        assert!(
            wd.free_atlas().is_none(),
            "negative control: a full reset evicts the shared atlas"
        );
    }

    /// Tier-1 binding for `CursorCatFold` after every forward and reverse leg;
    /// the pure footprint emitter, a mutant that switches edges on glass, is
    /// rejected by the healthy model and admitted by `Buggy=1`.
    #[test]
    fn cursor_cat_fold_conformance_real_placement_and_teleport_mutant() {
        use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

        let model = aterm_models_effects::cursor_cat_fold_model();
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let t0 = Instant::now();

        let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion);

        let sprite_place = |sprite: FreeSprite| {
            let wholly_left = sprite.x + i32::from(sprite.w) <= 0;
            let wholly_right = sprite.x >= grid_w;
            let off = wholly_left || wholly_right;
            let side = if wholly_left {
                0
            } else if wholly_right {
                1
            } else {
                i64::from(sprite.x * 2 + i32::from(sprite.w) >= grid_w)
            };
            (side, i64::from(off))
        };

        let project = |placement: KittyCursorPlacement,
                       sprite: FreeSprite,
                       origin_side: i64,
                       seen_off: bool,
                       folds: i64| {
            let (side, off_glass) = sprite_place(sprite);
            let (phase, direction) = match placement.leg {
                KittyCursorPlacementLeg::Settled => (0, 0),
                KittyCursorPlacementLeg::Exit { direction, .. } => (
                    if off_glass == 1 { 2 } else { 1 },
                    match direction {
                        CatFoldDirection::Forward => 1,
                        CatFoldDirection::Reverse => 2,
                    },
                ),
                KittyCursorPlacementLeg::Enter { direction, .. } => (
                    if off_glass == 1 { 3 } else { 4 },
                    match direction {
                        CatFoldDirection::Forward => 1,
                        CatFoldDirection::Reverse => 2,
                    },
                ),
            };
            let mut state = model.init_state();
            state.insert("phase", phase);
            state.insert("direction", direction);
            state.insert("origin_side", origin_side);
            state.insert("side", side);
            state.insert("off_glass", off_glass);
            state.insert("off_samples", i64::from(seen_off));
            state.insert("folds", folds);
            state
        };

        let assert_step = |before: &std::collections::BTreeMap<&'static str, i64>,
                           after: &std::collections::BTreeMap<&'static str, i64>,
                           action: &'static str,
                           label: &str| {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                before,
                after,
                Some(action),
                label,
            );
            assert!(ok, "{label}: {why}");
            let mut expected = before.clone();
            assert!(model.fire(action, &mut expected), "{label}: action enabled");
            assert_eq!(&expected, after, "{label}: real projection matches model");
        };

        let motion = |started: Instant, direction, progress: f32| CursorCatPlacementFrame {
            sampled_at: started + Duration::from_secs_f32(progress * 0.28),
            fold: Some(CatFoldFrame {
                started,
                direction,
                progress,
            }),
            facing_left: matches!(direction, CatFoldDirection::Reverse),
        };

        let mut wd = WordDecorations::default();
        let (seed_placement, seed) = emit(&mut wd, (3, g.cols - 1), settled_at(t0));
        let mut state = project(seed_placement, seed, 1, false, 0);
        assert_eq!(state, model.init_state(), "right-edge seed is model Init");

        let forward_at = t0 + Duration::from_millis(16);
        for (progress, action, origin, seen_off, label) in [
            (0.0, "StartForward", 1, false, "forward launch on old edge"),
            (0.499, "LeaveOff", 1, true, "forward body wholly right-off"),
            (0.5, "CrossSide", 1, true, "forward body wholly left-off"),
            (0.9, "EnterGlass", 1, true, "forward arrival on new edge"),
        ] {
            let (placement, sprite) = emit(
                &mut wd,
                (4, 0),
                motion(forward_at, CatFoldDirection::Forward, progress),
            );
            let next = project(placement, sprite, origin, seen_off, 1);
            assert_step(&state, &next, action, label);
            state = next;
        }
        let (placement, sprite) = emit(
            &mut wd,
            (4, 0),
            settled_at(forward_at + crate::kitty_cursor::CURSOR_FOLD_DURATION),
        );
        let next = project(placement, sprite, 0, false, 1);
        assert_step(&state, &next, "Finish", "forward settles on left edge");
        state = next;

        let reverse_at =
            forward_at + crate::kitty_cursor::CURSOR_FOLD_DURATION + Duration::from_millis(16);
        for (progress, action, origin, seen_off, label) in [
            (0.0, "StartReverse", 0, false, "reverse launch on old edge"),
            (0.499, "LeaveOff", 0, true, "reverse body wholly left-off"),
            (0.5, "CrossSide", 0, true, "reverse body wholly right-off"),
            (0.9, "EnterGlass", 0, true, "reverse arrival on new edge"),
        ] {
            let (placement, sprite) = emit(
                &mut wd,
                (3, g.cols - 1),
                motion(reverse_at, CatFoldDirection::Reverse, progress),
            );
            let next = project(placement, sprite, origin, seen_off, 2);
            assert_step(&state, &next, action, label);
            state = next;
        }
        let (placement, sprite) = emit(
            &mut wd,
            (3, g.cols - 1),
            CursorCatPlacementFrame {
                sampled_at: reverse_at + crate::kitty_cursor::CURSOR_FOLD_DURATION,
                fold: None,
                facing_left: true,
            },
        );
        let next = project(placement, sprite, 1, false, 2);
        assert_step(&state, &next, "Finish", "reverse settles on right edge");

        // Negative control: the pure placement law switches sides with both
        // frames on glass.
        let mut mutant_wd = WordDecorations::default();
        let mut free = Vec::new();
        let frame = |cursor| still_frame(g, cursor);
        mutant_wd
            .kitty_cursor(frame((3, g.cols - 1)), &mut free)
            .expect("old edge emits");
        let old_edge = *free.last().expect("old body");
        free.clear();
        mutant_wd
            .kitty_cursor(frame((4, 0)), &mut free)
            .expect("new edge emits");
        let new_edge = *free.last().expect("new body");
        assert_eq!(sprite_place(old_edge), (1, 0));
        assert_eq!(sprite_place(new_edge), (0, 0));

        let mut before = model.init_state();
        assert!(model.fire("StartForward", &mut before));
        let mut teleported = before.clone();
        teleported.insert("phase", 4);
        teleported.insert("side", 0);
        teleported.insert("off_glass", 0);
        teleported.insert("off_samples", 0);
        let (healthy_ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &teleported,
            Some("LeaveOff"),
            "direct edge teleport mutant",
        );
        assert!(
            !healthy_ok,
            "healthy model rejects the old direct placement"
        );
        assert!(!model.check_invariant("OffGlassBeforeSideChange", &teleported));

        let mut buggy = aterm_models_effects::cursor_cat_fold_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let (buggy_ok, why) = aterm_spec::verify::validate_transition_tiered(
            &buggy,
            &[],
            &before,
            &teleported,
            Some("LeaveOff"),
            "Buggy=1 direct edge teleport",
        );
        assert!(buggy_ok, "Buggy=1 must reproduce the mutant: {why}");
    }

    /// When the right-edge clamp pushes the body back over the cursor cell it
    /// rises half a cell; at the top edge the rise backs off.
    #[test]
    fn right_edge_clamp_rises_the_companion_instead_of_covering_the_text() {
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let wd = WordDecorations::default();
        let fp_at = |cursor: (u16, u16)| {
            wd.kitty_cursor_footprint(still_layout(g, cursor))
                .expect("visible footprint")
        };
        let interior = fp_at((3, 5));
        let edge = fp_at((3, g.cols - 1));
        assert_eq!(edge.x + i32::from(edge.w), grid_w, "grid-edge clamp holds");
        let cursor_right = i32::from(g.cols) * i32::from(g.cell_w);
        assert!(
            edge.x < cursor_right,
            "the clamped sprite horizontally overlaps the text cells"
        );
        assert_eq!(
            edge.y,
            interior.y - i32::from(g.cell_h) / 2,
            "clamped footprint rises ch/2 above the centred rest"
        );

        // Shipping emission follows the same placement and stays body-only.
        let mut wd = WordDecorations::default();
        let emit = |wd: &mut WordDecorations, cursor: (u16, u16)| {
            let mut free = Vec::new();
            wd.kitty_cursor(still_frame(g, cursor), &mut free);
            assert_eq!(free.len(), 1, "one companion body at {cursor:?}");
            free[0]
        };
        let interior_body = emit(&mut wd, (3, 5));
        let edge_body = emit(&mut wd, (3, g.cols - 1));
        assert_eq!(
            edge_body.y,
            interior_body.y - i32::from(g.cell_h) / 2,
            "the emitted body follows the edge-rise footprint"
        );

        let top_interior = fp_at((0, 5));
        let top_edge = fp_at((0, g.cols - 1));
        assert_eq!(
            top_edge.y, top_interior.y,
            "a top-row flight is never lifted off-grid by the rise"
        );
    }

    /// The margin staircase glides: typing a key per 16 ms frame across the
    /// clamp region moves the body only a few px per frame (`PLACE_TAU`), and
    /// it lands on the raw placement's exact bytes once keys stop.
    #[test]
    fn margin_staircase_glides_instead_of_stepping_per_key() {
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let emit =
            |wd: &mut WordDecorations, cursor, at| emit_placed(wd, g, cursor, settled_at(at)).1;

        // The first emission snaps by law; it is the baseline, not a step.
        let mut at = t0;
        let mut prev = emit(&mut wd, (3, 15), at);
        let mut max_step = 0i32;
        for ccol in 16..=19u16 {
            at += Duration::from_millis(16);
            let body = emit(&mut wd, (3, ccol), at);
            max_step = max_step.max((body.y - prev.y).abs());
            prev = body;
        }
        assert!(
            max_step <= 4,
            "typing into the margin must glide, not step: max per-frame |dy| = {max_step} px"
        );

        // Once the keys stop, the follower converges onto the exact raw law.
        let mut settled = prev;
        for _ in 0..40 {
            at += Duration::from_millis(16);
            settled = emit(&mut wd, (3, 19), at);
        }
        let mut direct = Vec::new();
        wd.kitty_cursor(still_frame(g, (3, 19)), &mut direct)
            .expect("direct emitter draws");
        assert_eq!(
            (settled.x, settled.y),
            (direct[0].x, direct[0].y),
            "the glide lands on the same bytes the raw placement law draws"
        );
    }

    /// The seat's lift glides: the companion router raising the head a row
    /// (its seat over inked text) and setting it back down moves the drawn body
    /// by at most the follower's step per 8 ms present, and it lands on the raw
    /// placement each time. Negative control: the same lift carried in the hover
    /// bob (as it was until 2026-10-20), which the follower excludes, moves the
    /// body a whole row in one present.
    #[test]
    fn a_seat_lift_glides_instead_of_jumping_a_row() {
        let g = geom20();
        let cursor = (3, 5);
        let raw = |wd: &WordDecorations, lift: f32| {
            wd.kitty_cursor_footprint(KittyCursorLayout {
                lift,
                ..still_layout(g, cursor)
            })
            .expect("raw law")
            .y
        };
        let run = |as_bob: bool| {
            let mut wd = WordDecorations::default();
            let mut at = Instant::now();
            let mut emit = |lifted: f32, at| {
                let (bob, lift) = if as_bob {
                    (-lifted, 0.0)
                } else {
                    (0.0, lifted)
                };
                let layout = KittyCursorLayout {
                    bob,
                    lift,
                    ..still_layout(g, cursor)
                };
                let placement = wd
                    .resolve_kitty_cursor_placement(layout, settled_at(at))
                    .expect("placeable companion");
                let mut free = Vec::new();
                wd.kitty_cursor_at_placement(
                    KittyCursorFrame {
                        bob,
                        lift,
                        ..still_frame(g, cursor)
                    },
                    placement,
                    &mut free,
                )
                .expect("companion emitted");
                free.last().expect("the body emits last").y
            };
            let mut prev = emit(0.0, at);
            let (mut max_step, mut landed) = (0, Vec::new());
            for lifted in [1.0f32, 0.0] {
                for _ in 0..40 {
                    at += Duration::from_millis(8);
                    let y = emit(lifted, at);
                    max_step = max_step.max((y - prev).abs());
                    prev = y;
                }
                landed.push(prev);
            }
            (max_step, landed)
        };
        let wd = WordDecorations::default();
        let (step, landed) = run(false);
        assert!(
            step <= 3,
            "a seat lift moved the body {step} px in one present (cell {} px)",
            g.cell_h
        );
        assert_eq!(
            landed,
            vec![raw(&wd, 1.0), raw(&wd, 0.0)],
            "it lands on the law"
        );
        assert_eq!(
            run(true).0,
            i32::from(g.cell_h),
            "control: a lift in the bob jumps a whole row"
        );
    }

    /// A fold started mid-glide exits from the body drawn last frame (the
    /// shared `last_sprite`), not the raw settled target.
    #[test]
    fn a_fold_started_mid_glide_exits_from_the_glide_position() {
        use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion).1;
        let settled = settled_at;

        // Cross the rise onset so the follower lags when the fold interrupts.
        let _ = emit(&mut wd, (3, 14), settled(t0));
        let _ = emit(&mut wd, (3, 15), settled(t0 + Duration::from_millis(16)));
        let mid = emit(&mut wd, (3, 16), settled(t0 + Duration::from_millis(32)));
        let raw = wd
            .kitty_cursor_footprint(still_layout(g, (3, 16)))
            .expect("raw law");
        assert_ne!(
            (mid.x, mid.y),
            (raw.x, raw.y),
            "negative control: the body is genuinely mid-glide, behind the raw law"
        );

        let started = t0 + Duration::from_millis(48);
        let launch = emit(
            &mut wd,
            (4, 0),
            CursorCatPlacementFrame {
                sampled_at: started,
                fold: Some(CatFoldFrame {
                    started,
                    direction: CatFoldDirection::Forward,
                    progress: 0.0,
                }),
                facing_left: false,
            },
        );
        assert_eq!(
            (launch.x, launch.y),
            (mid.x, mid.y),
            "the Exit leg is C0-continuous with the glide position"
        );
    }

    /// The clockless [`WordDecorations::kitty_cursor`] path draws the raw
    /// footprint and never touches the follower, even mid-glide.
    #[test]
    fn compat_settled_emission_is_the_raw_footprint_and_ignores_the_glide() {
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let resolver_emit =
            |wd: &mut WordDecorations, cursor, at| emit_placed(wd, g, cursor, settled_at(at)).1;

        // Put a live glide on the ledger.
        let _ = resolver_emit(&mut wd, (3, 16), t0);
        let _ = resolver_emit(&mut wd, (3, 17), t0 + Duration::from_millis(16));
        let ledger_settle = wd.kitty_cursor_placement.settle;
        let ledger_sprite = wd.kitty_cursor_placement.last_sprite;
        assert!(ledger_settle.is_some(), "a resolver glide is live");

        let raw = wd
            .kitty_cursor_footprint(still_layout(g, (3, 17)))
            .expect("raw law");
        let compat = |wd: &mut WordDecorations| {
            let mut free = Vec::new();
            wd.kitty_cursor(still_frame(g, (3, 17)), &mut free)
                .expect("compat emitter draws");
            *free.last().expect("one body")
        };
        let first = compat(&mut wd);
        let second = compat(&mut wd);
        assert_eq!(
            (first.x, first.y),
            (raw.x, raw.y),
            "the compat path draws the raw footprint, not the glide"
        );
        assert_eq!(first, second, "the compat path is byte-stable");
        assert_eq!(
            wd.kitty_cursor_placement.settle, ledger_settle,
            "the compat path never writes the follower's ledger"
        );
        assert_eq!(
            wd.kitty_cursor_placement.last_sprite, ledger_sprite,
            "the compat path never writes the continuity ledger"
        );
    }

    #[test]
    fn invalid_kitty_source_fails_closed_instead_of_using_builtin_art() {
        let g = geom20();
        let mut wd = WordDecorations::default();
        wd.set_kitty_sprite_source(KittySpriteSource::Disabled);
        assert_eq!(wd.kitty_sprite_source_fingerprint(), None);
        assert!(wd.kitty_cursor_footprint(still_layout(g, (3, 5))).is_none());
        let mut free = Vec::new();
        assert!(
            wd.kitty_cursor(still_frame(g, (3, 5)), &mut free,)
                .is_none()
        );
        assert!(free.is_empty());
    }

    /// A Bow cat's sighting carries `TRAIT_BOW`, which the kitty-log accessory
    /// counters key off (`accessory_trait_bits`).
    #[test]
    fn bow_cat_sighting_carries_trait_bow() {
        use crate::kitty_registry::{TRAIT_BOW, accessory_trait_bits};
        let now = Instant::now();
        // No magic build (`% 4096` = 100); the accessory window decodes Bow.
        let bow = (64u64 << 24) | 100;
        assert_eq!(accessory_trait_bits(genome::cat_accessory(bow)), TRAIT_BOW);
        // Control: the bare twin's accessory window misses.
        let bare = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now).genome.magic;
        for (label, magic, bit) in [("bow", bow, TRAIT_BOW), ("bare", bare, 0)] {
            let mut o = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
            o.genome.magic = magic;
            let mut e = Episode::fresh(o.appeared, o.genome, o.seed, o.row, 0);
            e.phase_start = Some(o.appeared);
            let mut wd = WordDecorations {
                occ: vec![o.clone()],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            wd.persist.insert(o.ident, e);
            // Drain across a few ticks (bakes land ≤ 2/frame).
            let mut s: Vec<KittySighting> = Vec::new();
            for k in 0..4u64 {
                let at = now + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
                tick_cat_at(&mut wd, at, &cfg(), geom20(), None, true);
                s.extend(wd.drain_kitty_sightings());
            }
            assert_eq!(s.len(), 1, "{label}: one sighting");
            assert_eq!(
                s[0].traits & TRAIT_BOW,
                bit,
                "{label}: the accessory rides the sighting trait bits, got {:#010b}",
                s[0].traits
            );
        }
    }

    // ───────────────────── curse bonk cue battery ─────────────────────

    /// A curse typed at the prompt records one `Typed` cue, once.
    #[test]
    fn typed_curse_birth_records_one_bonk_cue() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 32);
        let mut wd = WordDecorations::default();
        term.process(b"fuck");
        // Typed: caret one past the token and a committed key.
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 4, 32, &lex, &c, 1, t0);
        let mut out = Vec::new();
        tick_deco(&mut wd, t0, &c, &mut out);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert_eq!(cues.len(), 1, "one typed curse ⇒ one cue");
        assert_eq!(cues[0].kind, CurseCueKind::Typed);
        assert_eq!(
            (cues[0].row, cues[0].col),
            (0, 0),
            "the cue carries the word's lead cell for stereo pan"
        );
        // One-shot: the same episode never re-cues on later ticks.
        tick_deco(&mut wd, t0 + Duration::from_millis(16), &c, &mut out);
        assert_eq!(wd.drain_curse_cues().count(), 0);
        // …and a passive redraw (fast-path adoption) births nothing.
        wd.rescan(&term, 4, 32, &lex, &c, 2, t0 + Duration::from_millis(32));
        tick_deco(&mut wd, t0 + Duration::from_millis(48), &c, &mut out);
        assert_eq!(wd.drain_curse_cues().count(), 0);
    }

    /// Typed zh / ja / ko / en curses each surface one typed cue.
    #[test]
    fn multilingual_typed_curses_bonk_zh_ja_ko_en() {
        let lex = Lexicon::with_languages(&["all"]);
        let c = cfg();
        let t0 = Instant::now();
        for word in ["fuck", "他妈的", "くそ", "씨발"] {
            let mut term = Terminal::new(2, 32);
            term.process(word.as_bytes());
            let mut wd = WordDecorations::default();
            // Typed: caret completion plus a committed key.
            wd.note_typed_edit(t0, None);
            wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
            let mut out = Vec::new();
            tick_deco(&mut wd, t0, &c, &mut out);
            let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
            assert_eq!(cues.len(), 1, "{word:?} must record one typed cue");
            assert_eq!(cues[0].kind, CurseCueKind::Typed, "{word:?}");
        }
    }

    /// Ignore lists silence the bonk with the decoration, for folded spaced
    /// surfaces and raw CJK compounds alike.
    #[test]
    fn ignore_lists_suppress_the_bonk_with_the_decoration() {
        let lex = Lexicon::with_languages(&["all"]);
        let t0 = Instant::now();
        for (word, ignored) in [("fuck", "fuck"), ("씨발", "씨발")] {
            let mut c = cfg();
            c.ignore.insert(ignored.to_string());
            let mut term = Terminal::new(2, 32);
            term.process(word.as_bytes());
            let mut wd = WordDecorations::default();
            wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Profanity),
                "{word:?} must not decorate under its ignore entry"
            );
            let mut out = Vec::new();
            tick_deco(&mut wd, t0, &c, &mut out);
            assert_eq!(
                wd.drain_curse_cues().count(),
                0,
                "{word:?} must not bonk under its ignore entry"
            );
        }
    }

    /// `dmg` never bonks: the bare `dm` skeleton of Vietnamese `đm` is held by
    /// the lexicon's marks-required gate, while the marked form cues once.
    #[test]
    fn typing_dmg_never_bonks_but_the_marked_vietnamese_form_does() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        // Negative arm: d, m, g one key at a time; no keystroke may cue.
        let mut term = Terminal::new(2, 32);
        let mut wd = WordDecorations::default();
        let mut out = Vec::new();
        for (i, byte) in b"dmg".iter().copied().enumerate() {
            term.process(&[byte]);
            wd.note_typed_edit(t0, None);
            wd.rescan(&term, 2, 32, &lex, &c, i as u64 + 1, t0);
            tick_deco(&mut wd, t0 + Duration::from_millis(i as u64), &c, &mut out);
            assert_eq!(
                wd.drain_curse_cues().count(),
                0,
                "prefix {:?} of dmg must never bonk",
                &b"dmg"[..=i]
            );
        }
        // Positive arm: the tone-marked form cues once.
        let mut term = Terminal::new(2, 32);
        let mut wd = WordDecorations::default();
        term.process("đm".as_bytes());
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
        tick_deco(&mut wd, t0, &c, &mut out);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert_eq!(cues.len(), 1, "typed đm must record one cue");
        assert_eq!(cues[0].kind, CurseCueKind::Typed);
    }

    /// A rolled supernova and a classic nova each cue `Detonated` once, at the
    /// grant; an output curse detonating cues no `Typed`.
    #[test]
    fn supernova_detonation_cues_at_the_grant_edge_once() {
        let lex = lex();
        let t0 = Instant::now();
        let g = geom20();
        for (profile, c) in [("supernova", cfg_rainbow(100)), ("nova", cfg_nova())] {
            // Typed curse: the same tick carries the typed cue and the grant cue.
            let mut term = Terminal::new(6, 20);
            term.process(b"oh fuck");
            let mut wd = WordDecorations::default();
            // Typed: caret completion plus a committed key (`wd2` below omits
            // it: the output case).
            wd.note_typed_edit(t0, None);
            wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
            tick_nova(&mut wd, t0, &c, g, None, None);
            let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
            assert!(
                cues.iter().any(|q| q.kind == CurseCueKind::Typed),
                "{profile}: typed witness cue missing: {cues:?}"
            );
            assert_eq!(
                cues.iter()
                    .filter(|q| q.kind == CurseCueKind::Detonated)
                    .count(),
                1,
                "{profile}: exactly one detonation cue at the grant: {cues:?}"
            );
            // Once per episode: the running blast re-cues nothing.
            tick_nova(&mut wd, t0 + Duration::from_millis(100), &c, g, None, None);
            assert_eq!(wd.drain_curse_cues().count(), 0, "{profile}");
            // Output curse (no typed witness): Detonated only.
            let mut term2 = Terminal::new(6, 20);
            term2.process(b"fuck\r\n$ ");
            let mut wd2 = WordDecorations::default();
            wd2.rescan(&term2, 6, 20, &lex, &c, 1, t0);
            tick_nova(&mut wd2, t0, &c, g, None, None);
            let cues2: Vec<CurseCue> = wd2.drain_curse_cues().collect();
            assert!(
                cues2.iter().all(|q| q.kind == CurseCueKind::Detonated),
                "{profile}: output content must never carry the typed kind: {cues2:?}"
            );
            assert_eq!(
                cues2.len(),
                1,
                "{profile}: the output detonation still cues: {cues2:?}"
            );
        }
    }

    /// Undrained cues clear at the next tick's start, and `reset()` drops them.
    #[test]
    fn curse_cues_clear_at_tick_start_and_on_state_drop() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 32);
        let mut wd = WordDecorations::default();
        term.process(b"fuck");
        // Typed: caret one past the token and a committed key.
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 4, 32, &lex, &c, 1, t0);
        let mut out = Vec::new();
        tick_deco(&mut wd, t0, &c, &mut out); // cue recorded, NOT drained
        assert_eq!(wd.curse_cues.len(), 1);
        tick_deco(&mut wd, t0 + Duration::from_millis(16), &c, &mut out);
        assert_eq!(
            wd.drain_curse_cues().count(),
            0,
            "an undrained frame's cues never accumulate (tick-start clear)"
        );
        // State drop: a fresh birth's pending cue dies with reset().
        let mut wd2 = WordDecorations::default();
        wd2.note_typed_edit(t0, None);
        wd2.rescan(&term, 4, 32, &lex, &c, 1, t0);
        tick_deco(&mut wd2, t0, &c, &mut out);
        assert_eq!(wd2.curse_cues.len(), 1);
        wd2.reset();
        assert_eq!(
            wd2.drain_curse_cues().count(),
            0,
            "reset() must drop pending cues with the state"
        );
    }

    /// Pane binding: each pane has its own grid and episode state; the flash
    /// limiter and supernova mutex stay window-wide.
    mod pane_binding {
        use super::*;

        /// Scan `text` into the currently bound pane at the given grid size.
        fn rescan_pane(
            wd: &mut WordDecorations,
            text: &str,
            rows: usize,
            cols: usize,
            c: &DecoConfig,
            epoch: u64,
            now: Instant,
        ) {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(text.as_bytes());
            let mut snap = aterm_core::render::RenderInput::default();
            term.cell_frame_into(&mut snap, rows, cols);
            let bg = snap
                .cells
                .iter()
                .find_map(|line| line.first())
                .map_or(0, |cell| rgb3_to_u32(cell.bg));
            let geom = EffectGeom {
                cell_w: 10,
                cell_h: 20,
                rows: rows as u16,
                cols: cols as u16,
            };
            wd.rescan_from_cells_with_geom_at_cursor(
                &snap.cells,
                &snap.line_sizes,
                rows,
                cols,
                &lex(),
                c,
                epoch,
                now,
                geom,
                bg,
                None,
            );
        }

        /// The feline episode of the bound pane, if it has one.
        fn feline_episode(wd: &WordDecorations) -> Option<&Episode> {
            let ident = wd
                .occ
                .iter()
                .find(|o| o.class == Class::Feline)
                .map(|o| o.ident)?;
            wd.persist.get(&ident)
        }

        /// `last_epoch` is per pane: a sibling's scan neither satisfies nor
        /// forces this pane's.
        #[test]
        fn bound_panes_keep_independent_damage_epochs() {
            let c = cfg();
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            wd.bind_pane(1, (0, 0));
            assert!(wd.needs_rescan(1), "a pane's first frame always scans");
            rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
            assert!(!wd.needs_rescan(1), "and not twice at the same epoch");

            wd.bind_pane(2, (0, 0));
            assert!(
                wd.needs_rescan(1),
                "pane 2 has scanned NOTHING — a shared last_epoch would starve \
                 it of its first scan for as long as pane 1 stays quiet"
            );
            rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);

            wd.bind_pane(1, (0, 0));
            assert!(
                !wd.needs_rescan(1),
                "pane 1's own epoch survived pane 2's scan"
            );
        }

        /// Alternating pane widths do not re-arm `settle_until` forever.
        #[test]
        fn unequal_pane_widths_do_not_pin_born_settled() {
            let c = cfg();
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            let text = "\r\nhello kitty friend\r\n";
            for round in 0..4u64 {
                wd.bind_pane(1, (0, 0));
                rescan_pane(&mut wd, text, 6, 40, &c, round + 1, t0);
                wd.bind_pane(2, (400, 0));
                rescan_pane(&mut wd, text, 6, 39, &c, round + 1, t0);
            }
            wd.bind_pane(1, (0, 0));
            assert!(
                wd.settle_until.is_none(),
                "pane 1's width never changed — the resize-settle window must \
                 be shut, not re-armed by its sibling's different width"
            );
            let ep = feline_episode(&wd).expect("pane 1 has a feline episode");
            assert!(
                !ep.born_settled,
                "a stable pane still PLAYS its entrance in a split"
            );
        }

        /// The same word at the same column in two panes gets its own episode in
        /// each.
        #[test]
        fn identical_words_in_two_panes_each_get_their_own_cat() {
            let c = cfg();
            let t0 = Instant::now();
            let t1 = t0 + Duration::from_millis(400);
            let text = "\r\nhello kitty friend\r\n";
            let mut wd = WordDecorations::default();

            wd.bind_pane(1, (0, 0));
            rescan_pane(&mut wd, text, 6, 20, &c, 1, t0);
            assert_eq!(
                feline_episode(&wd).expect("pane 1 birth").appeared,
                t0,
                "pane 1's cat is born when pane 1 first saw the word"
            );

            wd.bind_pane(2, (200, 0));
            rescan_pane(&mut wd, text, 6, 20, &c, 1, t1);
            assert_eq!(
                feline_episode(&wd).expect("pane 2 birth").appeared,
                t1,
                "pane 2's identical word is its OWN birth — a shared persist map \
                 would hand it pane 1's already-running (or already-spent) episode"
            );

            wd.bind_pane(1, (0, 0));
            assert_eq!(
                feline_episode(&wd).expect("pane 1 survives").appeared,
                t0,
                "and pane 1's episode is untouched by its sibling"
            );
        }

        /// Per-pane rescan sequences: a shared counter would close the
        /// `REKEY_MAX_SEQ_GAP` window within one frame at three panes.
        #[test]
        fn pane_ticks_do_not_inflate_rescan_seq() {
            let c = cfg();
            let t0 = Instant::now();
            let text = "\r\nhello kitty friend\r\n";
            let mut wd = WordDecorations::default();
            for pane in 1..=3u64 {
                wd.bind_pane(pane, (0, 0));
                rescan_pane(&mut wd, text, 6, 20, &c, 1, t0);
            }
            wd.bind_pane(1, (0, 0));
            assert_eq!(
                wd.rescan_seq, 1,
                "one pane, one scan, one sequence step — siblings must not \
                 consume pane 1's REKEY_MAX_SEQ_GAP budget"
            );
        }

        /// One `begin_host_frame` keeps a four-pane window at two bakes.
        #[test]
        fn one_host_frame_bakes_at_most_two_tiles_across_panes() {
            let c = cfg();
            let g = geom20();
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            for pane in 1..=3u64 {
                wd.bind_pane(pane, (i32::try_from(pane).expect("small") * 200, 0));
                rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
            }
            let t1 = t0 + Duration::from_millis(600);
            wd.begin_host_frame();
            let clock0 = wd.cat_baker.frame_clock();
            let mut cats = 0usize;
            for pane in 1..=3u64 {
                wd.bind_pane(pane, (i32::try_from(pane).expect("small") * 200, 0));
                let (free, _, _) = tick_cat(&mut wd, t1, &c, g);
                cats += free.len();
            }
            assert!(cats > 0, "non-vacuous: the panes really did emit cats");
            assert_eq!(
                wd.cat_baker.frame_clock(),
                clock0 + 1,
                "ONE baker prologue — so ONE bake budget — for the whole frame"
            );
        }

        /// The flash limiter is window-wide: same pane-local spots in two panes
        /// do not overlap-tighten, and a third flash in the second is deferred.
        #[test]
        fn flash_limiter_stays_window_wide_across_panes() {
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            let grant = |wd: &mut WordDecorations, owner: u64| {
                let scope = wd.scan_scope();
                grant_pane_ignition(
                    &mut wd.ignitions,
                    scope,
                    wd.pane_px,
                    owner,
                    t0,
                    (100, 100),
                    88.0,
                )
            };
            wd.bind_pane(1, (0, 0));
            let a = grant(&mut wd, 11).expect("capacity");
            wd.bind_pane(2, (400, 0));
            let b = grant(&mut wd, 22).expect("capacity");
            assert_eq!(
                (a, b),
                (t0, t0),
                "same pane-local centre, DIFFERENT panes — the overlap test \
                 must compare window pixels or it tightens the cap to one"
            );
            wd.bind_pane(3, (800, 0));
            let c = grant(&mut wd, 33).expect("capacity");
            assert_eq!(
                c,
                t0 + IGNITION_WINDOW,
                "and the rolling-second cap of two is counted over ALL panes"
            );
        }

        /// One pane's prune does not cancel a sibling's queued nova (its owner
        /// is parked out of `persist`; the pane label protects it).
        #[test]
        fn a_panes_pending_ignition_survives_another_panes_tick() {
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            wd.bind_pane(1, (0, 0));
            for (owner, cx) in [(1u64, 100i32), (2, 900), (3, 1700)] {
                let scope = wd.scan_scope();
                grant_pane_ignition(
                    &mut wd.ignitions,
                    scope,
                    wd.pane_px,
                    owner,
                    t0,
                    (cx, 100),
                    0.0,
                );
            }
            let pending = wd
                .ignitions
                .iter()
                .find(|r| r.start > t0)
                .map(|r| r.start)
                .expect("the third grant is deferred past the rolling window");

            wd.bind_pane(2, (400, 0));
            wd.prune_ignitions(t0 + Duration::from_millis(10));
            assert!(
                wd.ignitions
                    .iter()
                    .any(|r| r.pane == Some(1) && r.start == pending),
                "pane 2's tick must not cancel pane 1's queued nova"
            );
        }

        /// A departed pane's state goes, and so does the live binding if it was
        /// that pane's.
        #[test]
        fn retain_panes_evicts_departed_panes() {
            let c = cfg();
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            for pane in 1..=3u64 {
                wd.bind_pane(pane, (0, 0));
                rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
            }
            wd.retain_panes(|s| s == 1);
            assert_eq!(
                wd.parked.keys().copied().collect::<Vec<_>>(),
                vec![1],
                "pane 2's parked state left with pane 2"
            );
            assert_eq!(wd.bound, None, "and pane 3's live binding with it");
            assert!(wd.occ.is_empty(), "a departed pane leaves no occurrences");
            // The survivor is untouched by the departed pane's reset.
            wd.bind_pane(1, (0, 0));
            assert!(
                feline_episode(&wd).is_some(),
                "the still-visible pane kept its episodes"
            );
            assert!(
                !wd.needs_rescan(1),
                "and its scan — a survivor is not re-scanned because a sibling closed"
            );
        }
    }
}

#[cfg(test)]
mod pet_handoff_emission_tests {
    use super::*;

    fn pet_frame() -> crate::kitty_pet::PetFrame {
        crate::kitty_pet::PetFrame {
            alpha: 255,
            lane_alpha: 255,
            action: crate::kitty_pet::PetAction::Sit,
            pose: crate::pet_glyphs_gen::PetGlyphId::PetSit,
            col: 10.0,
            row: 5.0,
            lift: 0.0,
            facing_left: false,
            scale_x: 1.0,
            scale_y: 1.0,
            purr: 0.0,
            contented: false,
            under_ink: false,
            motes: [None; crate::kitty_pet::PET_MOTES_MAX],
            departures: [None; crate::kitty_pet::PET_DEPARTURES_MAX],
        }
    }

    /// Under bake pressure a look flip refuses the old look's held tile
    /// ([`WordDecorations::pet_last_tile`]); the new look lands next frame.
    #[test]
    fn a_look_flip_refuses_the_old_looks_held_tile_under_bake_pressure() {
        let mut wd = WordDecorations::default();
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 24,
            cols: 80,
        };
        let colors = CatColorKey::default();
        let mut free = Vec::new();
        let dress = |wd: &mut WordDecorations, coat: u8, iris: u8, free: &mut Vec<_>| {
            free.clear();
            wd.pet_cursor(
                PetCursorFrame {
                    geom,
                    colors,
                    coat,
                    iris,
                    pet: pet_frame(),
                    page: None,
                },
                free,
            );
        };
        // Look A lands (the two-door bake may take a couple of frames).
        let mut landed = false;
        for _ in 0..8 {
            wd.cat_baker_ready = false;
            dress(&mut wd, 3, 1, &mut free);
            if !free.is_empty() {
                landed = true;
                break;
            }
        }
        assert!(landed, "look A's tile never landed");
        assert!(
            wd.pet_last_tile.is_some_and(|(.., c, i)| (c, i) == (3, 1)),
            "the held tile carries look A"
        );
        // A frame whose bake budget is spent, and the worn pair flips: the new
        // look cannot bake and the old look's hold is refused.
        wd.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
        wd.animal_baker.begin_frame(geom.cell_w, geom.cell_h);
        wd.cat_baker_ready = true;
        let scratch = vec![0u8; 4 * 4 * 4];
        assert!(
            wd.cat_baker
                .host_tile(0xDEAD_0001, 4, 4, &scratch)
                .is_some()
        );
        assert!(
            wd.cat_baker
                .host_tile(0xDEAD_0002, 4, 4, &scratch)
                .is_some()
        );
        dress(&mut wd, 9, 4, &mut free);
        assert!(
            free.is_empty(),
            "the old coat's held tile must not dress the new look"
        );
        // The budget returns: the new look bakes, draws and takes the hold.
        let mut landed = false;
        for _ in 0..8 {
            wd.cat_baker_ready = false;
            dress(&mut wd, 9, 4, &mut free);
            if !free.is_empty() {
                landed = true;
                break;
            }
        }
        assert!(landed, "look B's tile never landed");
        assert!(
            wd.pet_last_tile.is_some_and(|(.., c, i)| (c, i) == (9, 4)),
            "the held tile now carries look B"
        );
    }
}

#[cfg(test)]
mod pet_stays_inside_the_grid_tests {
    use super::*;

    const GEOM: EffectGeom = EffectGeom {
        cell_w: 9,
        cell_h: 19,
        rows: 24,
        cols: 80,
    };

    /// A resident pet on row 0 with a sleep `z` at its head anchor (above the
    /// grid before clamping).
    fn sleeper_on_the_top_row() -> crate::kitty_pet::PetFrame {
        let mut motes = [None; crate::kitty_pet::PET_MOTES_MAX];
        motes[0] = Some(crate::kitty_pet::PetMoteSprite {
            kind: crate::kitty_pet::PetMoteKind::Zee,
            col: 12.06,
            row: -0.66,
            scale: 0.58,
            rot: 0.4,
            alpha: 200,
            rgb: None,
        });
        crate::kitty_pet::PetFrame {
            alpha: 255,
            // A settled sleeper: lane and body alpha agree.
            lane_alpha: 255,
            action: crate::kitty_pet::PetAction::Sleep,
            pose: crate::pet_glyphs_gen::PetGlyphId::PetSleep0,
            col: 13.0,
            row: 0.0,
            lift: 0.0,
            facing_left: false,
            scale_x: 1.0,
            scale_y: 1.0,
            purr: 0.0,
            contented: false,
            under_ink: false,
            motes,
            departures: [None; crate::kitty_pet::PET_DEPARTURES_MAX],
        }
    }

    /// A cell-metric change (which wipes the shared atlas) drops every held
    /// tile.
    #[test]
    fn a_metric_change_drops_every_held_tile() {
        let mut wd = WordDecorations::default();
        wd.drop_holds_if_metric_moved(GEOM.cell_w, GEOM.cell_h);

        // Stand in for a frame that resolved every hold.
        wd.pet_last_tile = Some((0, 0, crate::pet_glyphs_gen::PetGlyphId::PetLoaf, 0, 0));
        wd.pet_depart_tiles[0] = Some((1, 0, 40));
        wd.dog_last_tile = Some((
            0,
            80,
            crate::dog_cameo::DogLook {
                breed: crate::dog_glyphs_gen::DogGlyphId::D1Beagle,
                coat: 0,
            },
        ));

        // Positive control: an unchanged metric keeps the holds.
        wd.drop_holds_if_metric_moved(GEOM.cell_w, GEOM.cell_h);
        assert!(
            wd.pet_last_tile.is_some() && wd.pet_depart_tiles[0].is_some(),
            "an unchanged metric keeps the holds"
        );

        // The font-size step: same atlas door, and every hold must go with it.
        wd.drop_holds_if_metric_moved(GEOM.cell_w * 2, GEOM.cell_h * 2);
        assert!(wd.pet_last_tile.is_none(), "the pet body's hold survived");
        assert!(
            wd.pet_depart_tiles.iter().all(Option::is_none),
            "a departing ghost's hold survived"
        );
        assert!(wd.dog_last_tile.is_none(), "the dog's hold survived");
    }

    /// A pet on row 0 emits no sprite above the grid (a negative y would land
    /// on the tab strip after the splice).
    #[test]
    fn a_pet_on_row_zero_emits_no_sprite_above_the_grid() {
        let mut wd = WordDecorations::default();
        let mut free: Vec<FreeSprite> = Vec::new();
        let pet = sleeper_on_the_top_row();
        let grid_w = i32::from(GEOM.cols) * i32::from(GEOM.cell_w);
        let grid_h = i32::from(GEOM.rows) * i32::from(GEOM.cell_h);
        // Drive frames until both body and mote land (two bakes per frame).
        let mut seen = 0usize;
        for _ in 0..12 {
            wd.begin_host_frame();
            free.clear();
            wd.pet_cursor(
                PetCursorFrame {
                    geom: GEOM,
                    colors: CatColorKey::default(),
                    coat: 3,
                    iris: 1,
                    pet,
                    page: None,
                },
                &mut free,
            );
            seen = seen.max(free.len());
            for s in &free {
                assert!(
                    s.y >= 0,
                    "a pet sprite resolved ABOVE the grid at y={} (h={}): after the \
                     tab-strip splice that is the strip, not padding",
                    s.y,
                    s.h
                );
                assert!(
                    s.y + i32::from(s.h) <= grid_h,
                    "a pet sprite resolved BELOW the grid: y={} h={} grid_h={grid_h}",
                    s.y,
                    s.h
                );
                assert!(
                    s.x >= 0 && s.x + i32::from(s.w) <= grid_w,
                    "…or off the sides"
                );
            }
        }
        assert!(
            seen >= 2,
            "fixture: the body AND the mote must both have been emitted (saw {seen} \
             sprites at most) or this test proves nothing"
        );
    }

    /// Mid-grid the clamp changes nothing: the emitted sprite is `body_px`.
    #[test]
    fn a_pet_in_open_grid_is_untouched_and_still_matches_body_px() {
        let mut wd = WordDecorations::default();
        let mut free: Vec<FreeSprite> = Vec::new();
        let pet = crate::kitty_pet::PetFrame {
            row: 9.0,
            motes: [None; crate::kitty_pet::PET_MOTES_MAX],
            ..sleeper_on_the_top_row()
        };
        let body = pet
            .body_px(GEOM.cell_w, GEOM.cell_h, GEOM.cols, GEOM.rows)
            .expect("a visible pet has a body");
        // Row 9 of 24 with a 1.7-row animal: nothing to clamp.
        assert_eq!(
            body.2,
            ((pet.row + 1.0) * f32::from(GEOM.cell_h)).round() as i32
                - (crate::kitty_pet::ART_ROWS * f32::from(GEOM.cell_h)).round() as i32,
            "an interior body keeps its exact unclamped top"
        );
        let mut drawn = None;
        for _ in 0..12 {
            wd.begin_host_frame();
            free.clear();
            wd.pet_cursor(
                PetCursorFrame {
                    geom: GEOM,
                    colors: CatColorKey::default(),
                    coat: 3,
                    iris: 1,
                    pet,
                    page: None,
                },
                &mut free,
            );
            if let Some(s) = free.first() {
                drawn = Some((s.x, s.y, s.w, s.h));
                break;
            }
        }
        let (x, y, w, h) = drawn.expect("the body tile lands within the retry window");
        assert_eq!(
            (x, y, i32::from(w), i32::from(h)),
            (body.0, body.2, body.1 - body.0, body.3 - body.2),
            "the emitted sprite must still BE `body_px`"
        );
    }
}

#[cfg(test)]
mod dog_cameo_emission_tests {
    use super::*;

    /// The dog cameo emits one free sprite behind the cursor, never over the
    /// cursor cell away from the left margin.
    #[test]
    fn dog_cameo_emits_one_trailing_sprite() {
        let mut wd = WordDecorations::default();
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 24,
            cols: 80,
        };
        let look = crate::dog_cameo::DogLook {
            breed: crate::dog_glyphs_gen::DOG_HEADS[0],
            coat: 9,
        };
        let frame = DogCameoFrame {
            geom,
            cursor: (5, 40),
            look,
            colors: CatColorKey::default(),
            bob: 0.0,
            alpha: 255,
        };
        let mut free = Vec::new();
        // The bake budget may defer the first frames; retry like the host.
        let mut fp = None;
        for frame_i in 0..8 {
            // Per-frame prologue the host's `tick` normally performs.
            wd.cat_baker_ready = false;
            free.clear();
            fp = wd.dog_cameo(frame, &mut free);
            if fp.is_some() {
                break;
            }
            assert!(frame_i < 7, "the dog tile never landed in 8 frames");
        }
        assert!(fp.is_some(), "the emission resolves a fingerprint");
        assert_eq!(free.len(), 1, "exactly one dog sprite");
        let s = &free[0];
        assert!(s.alpha == 255);
        let cursor_left = 40 * 10;
        assert!(
            s.x + i32::from(s.w) <= cursor_left,
            "the dog trails BEHIND the cursor cell (x {} w {} vs cursor left {cursor_left})",
            s.x,
            s.w
        );
        assert!(s.w > 0 && s.h > 0);
    }

    /// A held dog tile is refused to another breed (a re-summon always changes
    /// breed); the same breed still falls back to it.
    #[test]
    fn a_held_dog_tile_is_refused_to_another_breed() {
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 24,
            cols: 80,
        };
        let breeds = crate::dog_glyphs_gen::DOG_HEADS;
        assert!(breeds.len() >= 2, "fixture: the parade needs two breeds");
        let look = |i: usize| crate::dog_cameo::DogLook {
            breed: breeds[i],
            coat: 9,
        };
        let frame = |l: crate::dog_cameo::DogLook, accent: u8| DogCameoFrame {
            geom,
            cursor: (5, 40),
            look: l,
            colors: CatColorKey {
                accent,
                background: 0,
            },
            bob: 0.0,
            alpha: 255,
        };
        let mut wd = WordDecorations::default();
        // Two fresh palettes spend the budget; the third takes the hold path.
        fn spend_budget_then(
            wd: &mut WordDecorations,
            warm: [DogCameoFrame; 2],
            ask: DogCameoFrame,
        ) -> Option<u64> {
            let mut free = Vec::new();
            wd.begin_host_frame();
            for f in warm {
                free.clear();
                wd.dog_cameo(f, &mut free);
            }
            free.clear();
            wd.dog_cameo(ask, &mut free)
        }
        // Warm the hold onto breed 0 (the first palette lands on frame one).
        let mut free = Vec::new();
        for accent in [0u8, 1] {
            wd.begin_host_frame();
            free.clear();
            wd.dog_cameo(frame(look(0), accent), &mut free);
        }
        // Warm-up palettes must be unseen (a repeat is an atlas hit).
        // Control: starved, same breed ⇒ the hold is used.
        assert!(
            spend_budget_then(
                &mut wd,
                [frame(look(0), 2), frame(look(0), 3)],
                frame(look(0), 7)
            )
            .is_some(),
            "a starved frame must still re-emit the held tile for its OWN breed \
             — without this the refusal below would prove nothing"
        );
        // Starved, another breed: no sprite rather than the wrong breed.
        assert!(
            spend_budget_then(
                &mut wd,
                [frame(look(0), 4), frame(look(0), 5)],
                frame(look(1), 8)
            )
            .is_none(),
            "a held tile from another breed was re-stamped as the new one"
        );
    }
}

/// The trick flash's engine hooks against private state: what `tick` arms and
/// what a reset leaves (the flash's own battery is in `trick_flash.rs`).
#[cfg(test)]
mod trick_flash_hook_tests {
    use super::*;
    use crate::trick_flash::FLASH_MS;

    fn tick(wd: &mut WordDecorations, now: Instant, ink: &mut Vec<InkCell>) -> u64 {
        let (mut out, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new());
        wd.tick(
            now,
            &DecoConfig::default(),
            EffectGeom::default(),
            None,
            None,
            true,
            &mut out,
            ink,
            &mut free,
            &mut nova,
        )
    }

    /// Type `typed`, note `word` as typed to the pet, echo the Space, rescan.
    fn flash(term: &mut Terminal, wd: &mut WordDecorations, typed: &str, word: &str, t0: Instant) {
        let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        term.process(typed.as_bytes());
        wd.rescan(term, 4, 40, &lex, &c, 1, t0);
        wd.note_trick_typed(t0, None, word, 0);
        term.process(b" ");
        wd.rescan(term, 4, 40, &lex, &c, 2, t0);
    }

    /// With no live flash (even a pending one), a wordless tick returns empty
    /// ink, fp 0 and a `None` deadline.
    #[test]
    fn a_wordless_screen_with_no_flash_still_returns_fp_zero_and_disarms() {
        let t0 = Instant::now();
        let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        let mut term = Terminal::new(4, 40);
        term.process(b"$ sit");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
        assert!(wd.occ.is_empty(), "`sit` is nobody's lexicon word");
        let stale = InkCell {
            row: 0,
            col: 0,
            color: [1, 2, 3],
        };
        for pending in [false, true] {
            if pending {
                wd.note_trick_typed(t0, None, "sit", 0);
            }
            wd.active_until = Some(t0 + Duration::from_secs(5));
            let mut ink = vec![stale];
            assert_eq!(tick(&mut wd, t0, &mut ink), 0, "pending = {pending}");
            assert!(ink.is_empty(), "pending = {pending}");
            assert_eq!(wd.active_until, None, "pending = {pending}");
        }
    }

    /// The flash only extends the tick's deadline: a longer feline glow keeps
    /// its end, and after it settles a new flash is the only thing armed.
    #[test]
    fn beside_other_ink_the_flash_extends_the_deadline_and_never_shortens_it() {
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 40);
        let mut wd = WordDecorations::default();
        flash(&mut term, &mut wd, "$ kitty sit", "sit", t0);
        assert!(
            !wd.occ.is_empty(),
            "`kitty` is an occurrence: the tail hook"
        );
        let mut ink = Vec::new();
        tick(&mut wd, t0 + Duration::from_millis(300), &mut ink);
        let glow_end = t0 + Duration::from_millis(FELINE_GLOW_MS);
        assert!(FELINE_GLOW_MS > u64::from(FLASH_MS));
        assert_eq!(wd.active_until, Some(glow_end), "the longer window stands");
        assert!(ink.iter().any(|cell| (8..=10).contains(&cell.col)));
        // After both, within grace: a settled feline word arms nothing…
        let later = t0 + Duration::from_secs(2);
        tick(&mut wd, later, &mut ink);
        assert_eq!(wd.active_until, None);
        // …so a second command on the same line is the flash's deadline alone.
        let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        term.process(b"jump");
        wd.rescan(&term, 4, 40, &lex, &c, 3, later);
        wd.note_trick_typed(later, None, "jump", 0);
        term.process(b" ");
        wd.rescan(&term, 4, 40, &lex, &c, 4, later);
        tick(&mut wd, later + Duration::from_millis(16), &mut ink);
        assert_eq!(
            wd.active_until,
            Some(later + Duration::from_millis(u64::from(FLASH_MS)))
        );
        let flashed: Vec<u16> = ink.iter().map(|cell| cell.col).collect();
        assert_eq!(flashed, [12, 13, 14, 15]);
    }

    /// The flash twin of `reset_clears_stale_ink_on_next_tick`: `reset()` drops
    /// a live flash, and rescanning the same text does not bring it back.
    #[test]
    fn reset_clears_a_live_trick_flash_on_the_next_tick() {
        let t0 = Instant::now();
        for hard in [false, true] {
            let mut term = Terminal::new(4, 40);
            let mut wd = WordDecorations::default();
            flash(&mut term, &mut wd, "$ sit", "sit", t0);
            let mut ink = Vec::new();
            tick(&mut wd, t0 + Duration::from_millis(200), &mut ink);
            assert_eq!(ink.len(), 3);
            assert!(wd.is_active(t0 + Duration::from_millis(200)));
            if hard {
                wd.hard_reset_words();
            } else {
                wd.reset();
            }
            let now = t0 + Duration::from_millis(216);
            assert_eq!(tick(&mut wd, now, &mut ink), 0, "hard = {hard}");
            assert!(
                ink.is_empty(),
                "stale flash ink must clear on the next tick"
            );
            assert!(!wd.is_active(now));
            assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Idle);
            let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
            wd.rescan(&term, 4, 40, &lex, &c, 3, now);
            assert_eq!(tick(&mut wd, now, &mut ink), 0, "a reset is not a re-arm");
            assert!(ink.is_empty());
        }
    }

    /// A frozen engine neither locates nor paints the flash, and thaw finds it
    /// where the clock left it (the flash is not thaw-shifted).
    #[test]
    fn a_frozen_engine_neither_finds_nor_paints_the_flash() {
        let t0 = Instant::now();
        let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        let mut term = Terminal::new(4, 40);
        term.process(b"$ sit");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
        wd.note_trick_typed(t0, None, "sit", 0);
        term.process(b" ");
        wd.freeze(t0);
        wd.rescan(&term, 4, 40, &lex, &c, 2, t0);
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
        let mut ink = Vec::new();
        assert_eq!(tick(&mut wd, t0, &mut ink), 0);
        assert!(ink.is_empty());
        let thawed = t0 + Duration::from_millis(100);
        wd.thaw(thawed);
        wd.rescan(&term, 4, 40, &lex, &c, 2, thawed);
        assert_eq!(wd.trick_flash_phase(thawed), TrickFlashPhase::Live);
        tick(&mut wd, thawed + Duration::from_millis(200), &mut ink);
        assert_eq!(ink.len(), 3);
    }
}

// Tier-1 binding for `NovaPhase` on the host engine, a child module so it can
// read the host's episodes. The `_tests.rs` name keeps its engine out of the
// shipped-code census.
#[cfg(test)]
#[path = "nova_host_conformance_tests.rs"]
mod nova_host_conformance;
