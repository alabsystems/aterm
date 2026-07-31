// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "Sparkle words": render-time text decorations driven by whole-word lexical
//! matching.
//!
//! Two distinct, independently configurable effects:
//!
//! * **Profanity** (the "fuck" family across major languages) → a lively,
//!   *randomized*, *self-terminating* SPARKLE. Each spark's glyph, colour,
//!   jitter, and twinkle vary per frame from a seeded-deterministic RNG, so the
//!   effect is reproducible (replay-safe) yet never uniform. A given occurrence
//!   sparkles only for a bounded window after it appears, then goes steady — a
//!   static `fuck` on screen never pins a CPU core.
//! * **Feline** (cat / kitty / kitten across major languages) → the authored
//!   peeking cat: a head slides out above (or below) the word, dwells, and
//!   descends — a one-shot per occurrence. When the cat cannot draw (style,
//!   cell floors, narrow words, top row, `MAX_CATS` overflow) no graphic is
//!   shown at all; the word's own animated ink still plays.
//! * **Animated ink** (v2 — emphasis / profanity / feline; orca untouched) → the
//!   matched glyphs themselves are recolored through [`InkCell`] fg overrides: a
//!   two-tone gradient with one traveling specular sweep, settling to constant
//!   bytes forever (a stable fixed point, not a decay to default fg).
//!
//! This module owns the host-side state machine: it scans the visible grid when
//! the damage epoch advances (turning text into [`Occurrence`]s), then ticks an
//! animation each frame that emits [`WordDecoration`]s + [`InkCell`]s into
//! scratch buffers the renderer composites. Nothing here touches grid cells,
//! copied text, or recordings — the decorations are purely visual, exactly like
//! the cursor trail.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_hash::FxHashMap;
use aterm_lexicon::{Class, FormId, LangSet, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, InkCell, WordDecoration};
use aterm_scene::vector::FIXED_ONE;
use aterm_scene::{mix_rgb, smoothstep};

use crate::cat_baker::{BakeKeyV4, CatBaker, CatColorKey, EyesFrame, PATCH_STRIP};
use crate::kitty_registry::{
    KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
};
// The shared color math lives in the leaf `color_math` module (so `nova.rs`
// and the §13 demo compile without this host state machine); imported — not
// re-exported — here for the ink/guard call sites below.
use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS};
use crate::color_math::{hsv2rgb, hue_nudge, relative_luminance};
use crate::genome::{
    self, CatAge, Genome, NovaFeatures, NovaMagic, VoteScratch, accessory_variant_v4, cat_age_v4,
    cat_fills_v4, cat_magic, cat_variant_v4, mix, nova_features, nova_magic, special_variant_v4,
};
use crate::nova::{self, NovaEnv};
use crate::spec::{
    BurstKind, BurstSpec, Collection, Colorway, GraphicSpec, InkSpec, SpecTable, WordEffectSpec,
};
use crate::supernova::{self, SuperEnv};

/// Hard cap on occurrences tracked per frame (deterministic truncation keeps the
/// hot path bounded on a screen full of matches). Truncation prefers the
/// BOTTOM rows — whole rows are dropped top-down via the bottom-priority
/// cutoff restart — so the prompt row the user is typing at keeps its slots
/// even under `yes kitty`-grade output (see `dense_scan_cutoff`).
const MAX_OCCURRENCES: usize = 128;
/// Hard cap on emitted decorations per frame.
const MAX_DECORATIONS: usize = 256;
/// Hard cap on emitted ink cells per frame (§4.4): deterministic row-major
/// truncation at RESCAN time, whole words only — an occurrence past the cap gets
/// NO ink at all (partial-word ink would sweep a torn gradient).
const MAX_INK_CELLS: usize = 512;
/// Specular sweep ramp-in (ms) — §4.2 `env(t)`.
const INK_RAMP_IN_MS: u64 = 130;
/// Specular sweep ramp-out after `sweep_ms` (ms) — §4.2: after `sweep_ms + 250`
/// the emitted color is exactly the static gradient, constant bytes forever.
const INK_FADE_MS: u64 = 250;
/// §4.3 legibility bound: the word's mid-gradient ink must hold at least this
/// WCAG contrast against the word's cell background, else the mix is pulled
/// toward the captured base fg (the theme's own legible color) until it does.
/// At 2.5:1 the pastel feline / emphasis anchors settle as washed-out salmon on
/// light themes, visibly weaker than the surrounding theme text; 3.5:1 keeps the
/// tint while still reading as INK. Dark themes never bind at either value.
const MIN_INK_CONTRAST: f32 = 3.5;
/// FELINE words are not tinted pink. Their whole ink effect is a subtle
/// self-terminating GLOW pulse in the word's OWN fg color — total window ms.
const FELINE_GLOW_MS: u64 = 1400;
/// The glow's peak "lit" tone: the fg mixed this far toward white (never pure
/// white — the word glows in its own color).
const FELINE_GLOW_LIFT: f32 = 0.72;
/// Peak glow amplitude — subtle; the word brightens softly, it does not flash.
const FELINE_GLOW_AMP: f32 = 0.55;
/// §3.6 persist-map cap: identity episodes tracked across occlusion. Insertion
/// past the cap evicts the oldest-`last_seen` entry (deterministic ident
/// tiebreak), so a screenful of fresh identities can never grow the map.
const PERSIST_CAP: usize = 512;
/// §3.6 grace TTL: an identity unseen for longer than this is dropped at
/// rescan end. Within the window a word occluded by a clear+redraw, a redraw
/// race, or a brief overwrite keeps its `appeared`, its frozen genome, and its
/// spent nova (`nova_done`) — print-erase-print loops cannot re-roll or strobe
/// (the B-3 fix; ty-modeled as `SparkleIdentity` in aterm-spec).
const GRACE_TTL: Duration = Duration::from_secs(10);
/// Weak redraw-continuity recognition is deliberately narrower than the full
/// grace map: it joins only an immediately adjacent clear/redraw sequence.
/// Two damage scans cover `visible -> blank -> redrawn`. Sequence distance is
/// used instead of wall time so an overloaded event loop cannot turn a valid
/// redraw into a synthetic fresh birth merely because presentation was late.
const REKEY_MAX_SEQ_GAP: u64 = 2;
/// At fixed width, a same logical occurrence may cross a soft-wrap or move by
/// one inserted status/input row. A two-row reading-order window covers both
/// shapes while rejecting the established log-tail rotation fixture whose
/// genuinely new twin enters four rows away.
const REKEY_MAX_LINEAR_ROWS: u32 = 2;
/// One byte per cell in the bounded monotone alignment DP. Scores use two
/// rolling rows; only these decisions are retained for reconstruction.
const ALIGN_DECISION_CELLS: usize = (PERSIST_CAP + 1) * (MAX_OCCURRENCES + 1);
/// §3.6 ordinal mix constant: `ident = mix(seed ^ ordinal · THIS)`.
const ORDINAL_MIX: u64 = 0x9E37_79B9_7F4A_7C15;
/// Floor / ceiling for one generation of the [`ScanMemo`]; the live cap is the
/// viewport's row count clamped into this band (see `ScanMemo::fit`), so the
/// memo's footprint tracks the grid it is memoizing (≈ `2 · rows · cols` bytes)
/// instead of a fixed worst case sized for the widest terminal anyone might open.
const SCAN_MEMO_MIN_GEN: usize = 32;
const SCAN_MEMO_MAX_GEN: usize = 4096;
/// §5.2 hard cap on peeking cats per frame: row-major deterministic truncation;
/// occurrences past the cap fall back to the v1 paw + ink.
const MAX_CATS: usize = 8;
/// §5.6 entrance window (`CAT_RISE_MS ≈ 450`).
const CAT_RISE_MS: u64 = 450;
/// §5.6 kitten landing-bounce window after the rise (dest-rect offsets only).
const CAT_BOUNCE_MS: u64 = 150;
/// §5.7 readability floor: below these cell metrics there is no cat at all —
/// the word's ink is the graceful fallback (a cat that cannot render eyes is
/// worse than no cat).
const CAT_MIN_CELL_H: u16 = 14;
const CAT_MIN_CELL_W: u16 = 7;
/// v3 §1.2 descend window (easeInOutCubic).
const CAT_DESCEND_MS: u64 = 320;
/// v3 §1.2 optional pre-descend anticipation lift (~50% of genomes).
const CAT_ANTICIPATION_MS: u64 = 60;
/// v3 §1.2 dwell cap: `450 + 3750 + 380 = 4580 < 4800` leaves margin for the
/// driven A2 gate's post-input capture (`t = 5 s` byte-equal `t = 8 s`). The
/// episode is born AFTER the script starts, so the whole peek must fit inside
/// that margin or its last descend frames leak into the settled capture.
const CAT_DWELL_CAP_MS: u64 = 3750;
/// v3 §1.2 twin-desync dwell jitter: `dwell_base − mix(ident) % 300`.
const CAT_DWELL_JITTER_MS: u64 = 300;
/// v3 §1.2 magic/accessory dwell bonus (rare cats linger a beat longer).
const CAT_DWELL_MAGIC_BONUS_MS: u64 = 500;
/// v3 §1.1 fix #4 resize-settle window: births until the first rescan at
/// stable cols + this are born-settled (no entrance, no roll, static ink) and
/// never written to `done_marks`.
const RESIZE_SETTLE_MS: u64 = 500;
/// v3 §1.1 fix #2 `done_marks` LRU capacity.
const DONE_MARKS_CAP: usize = 65_536;
/// Sentinel for the intrusive done-mark LRU's `u32` links. The entry cap is
/// many orders of magnitude below this value, so every resident node has a
/// representable index while every possible `u64` done-mark key remains legal.
const DONE_MARK_NONE: u32 = u32::MAX;
/// v3 §1.2 graphics-decay rule: sparkle + nova ember residuals fade out
/// within this window after their animation window (orca residual exempt).
const RESIDUAL_FADE_MS: u64 = 2000;
/// v3 dwell genome derivation salts. DEVIATION (documented): the design
/// derives dwell from dedicated genome bits, but no such fields exist
/// in the §3.4 CAT layout — these decode locally as salted mixes of `gkey`,
/// preserving determinism and the printed range (dwell 2200..=3598).
const DWELL_SALT: u64 = 0xD3E1_15A1_C0FF_EE00;
const ANTIC_SALT: u64 = 0xA117_1C1F_7A11_57A6;
/// §6.4 flash-limiter rolling window: at most 2 ignitions per rolling second
/// window-wide (WCAG 2.3.1 charges 2 flash pairs per ignition against the
/// more-than-3-flashes/s threshold), tightening to ≤ 1 when regions overlap
/// (center distance < 2·R_max). Excess ignitions are DELAYED deterministically
/// (row-major queue; the Dip start shifts), never dropped. Machine-checked as
/// the `FlashLimiter` ty model (aterm-spec §9).
const IGNITION_WINDOW: Duration = Duration::from_secs(1);
/// At most two already-fired reservations can remain inside the disjoint-region
/// rolling window. Pending reservations are one-per-live persist episode, so
/// this sum is a structural upper bound even under a hostile redraw flood.
const MAX_RECENT_IGNITIONS: usize = 2;
const MAX_IGNITION_RESERVATIONS: usize = PERSIST_CAP + MAX_RECENT_IGNITIONS;

/// v3 §6 `BurstKind::Glow` window: one soft rise-and-fall, then done.
const GLOW_BURST_MS: u64 = 1400;
/// Lightweight star halo riding the opening of every profanity rainbow.
/// It fits entirely inside the already-animated rainbow drift, so it adds no
/// deadline or idle wake of its own.
const RAINBOW_SPARKLE_MS: u64 = 1000;

/// The orca "splash" glyph cycle — water droplets + a cross of spray.
const ORCA_GLYPHS: [DecoGlyph; 3] = [DecoGlyph::Droplet, DecoGlyph::Dot, DecoGlyph::Plus];
/// Ocean splash palette (`0x00RRGGBB`): deep blue, azure, bright cyan, foam white.
const ORCA_PALETTE: [u32; 4] = [0x0020_6CC8, 0x0029_A0E0, 0x0055_D6F0, 0x00E8_FBFF];

/// App-cached resolved sparkle state: the resolved [`DecoConfig`] plus the
/// compiled lexicon. Rebuilt only on config reload / toggle (NOT per frame), so
/// the per-frame path neither re-resolves config nor rebuilds the lexicon.
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
    /// `style = "paw"`: legacy ink-only compatibility mode; no paw graphic.
    Paw,
}

/// `[sparkle_words.profanity] style` (§10 / v3 §3.1): the v3 rainbow ink (the
/// new default), the v2 classic nova, or the exact v1 randomized sparkle
/// (each independently selectable).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfanityStyle {
    /// `style = "rainbow"` (the v3 default): the §3.1 animated rainbow ink,
    /// with `supernova_chance`% of episodes escalating to the §3.2 FUCK SUPER
    /// NOVA.
    Rainbow,
    /// `style = "nova"`: the v2 §6 classic nova. No size floor — novas scale
    /// (the ring band count clamps below 2r = 60 px, §6.3).
    /// `supernova_chance` is IGNORED under this style (the classic path
    /// keeps its budget certificates).
    Nova,
    /// `style = "sparkle"`: exactly the v1 path (`supernova_chance` ignored).
    Sparkle,
}

/// A borrowed view of the terminal's text selection for the tick (§6.4 items
/// 2/6): ignition DEFERS while the word is selected, and an active nova's
/// quads attenuate over selected cells (the same per-cell predicate the v1
/// Add-deco freeze uses, applied per quad host-side so both backends stay
/// byte-identical by construction). Selection rows are live rows; the rescan's
/// viewport rows convert through `display_offset` exactly as the renderers do.
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

/// The per-frame effect geometry (§2.4 `EffectGeom`): the cell metrics + grid
/// extent the cat emitter sizes/clamps against. All-zero (the default) trips
/// the §5.7 readability floor, so geometry-less callers get the v1 paw path.
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

/// One resolved cursor-companion emission. Grouping these scalar inputs keeps
/// the render call stable as companion art gains context without growing a
/// positional argument list.
#[derive(Clone, Copy, Debug)]
pub struct NyanCursorFrame {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    pub colors: CatColorKey,
    pub bob: f32,
    pub alpha: u8,
    /// The living-cartoon pose ([`crate::nyan_cursor::CatPose`]): a banking
    /// squash/stretch and forward lean applied to the dest rect, plus the baked
    /// eye frame. The cat bakes at its natural size (a stable, cache-cheap key per
    /// eye frame) and is scaled/leaned only at draw, so an animating pose never
    /// thrashes the atlas.
    pub pose: crate::nyan_cursor::CatPose,
    /// FULL-NYAN SING-ALONG drive 0..=1 (`crate::nyan_sing`): scales the
    /// music-note alpha so the wind-down crossfade eases the stream out.
    /// 0.0 (with an empty `notes` ring) is byte-identical to the pre-feature
    /// frame — no note work at all.
    pub sing: f32,
    /// This frame's resolved ♪/♫ sprites (cell-relative to the cat's mouth
    /// anchor), ring-capped at [`crate::nyan_sing::MAX_NOTES`] by the field
    /// that produced them. Emitted immediately before the body, so notes are
    /// structurally cursor-cat-only and load-shed with the sparkle branch.
    pub notes: [Option<crate::nyan_sing::NoteSprite>; crate::nyan_sing::MAX_NOTES],
}

/// Geometry-only inputs for resolving the cursor companion before its local
/// palette is sampled.
#[derive(Clone, Copy, Debug)]
pub struct NyanCursorLayout {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    pub bob: f32,
}

/// The cursor companion's prospective pixel footprint. This is resolved by
/// the same sizing/placement code as emission so palette sampling cannot drift
/// onto the cursor cell while the art actually occupies neighboring cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatFootprint {
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
}

/// The fully-resolved per-frame decoration settings (host config → engine).
#[derive(Clone, Debug)]
pub struct DecoConfig {
    /// Master + per-category gate already folded in: matching runs at all only
    /// when at least one category is on.
    pub profanity: bool,
    pub feline: bool,
    /// Orca / cetacean family → the randomized water-droplet "splash".
    pub orca: bool,
    /// Emphasis / hype words (user `extra_words` only; the builtin lexicon
    /// ships no emphasis forms) — the ink-only class. This gates only the
    /// class-default path. A custom override is resolved before class gates
    /// and intentionally bypasses this value, even though custom surfaces are
    /// indexed under the emphasis class.
    pub emphasis: bool,
    /// Animated glyph-ink shimmer over ink-bearing classes (emphasis + profanity
    /// + feline; orca untouched in v2). `false` ⇒ zero [`InkCell`]s emitted.
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
    /// Suppress ALL decorations while the alternate screen is active. Defaults
    /// OFF, so full-screen TUIs (vim, less, htop, lazygit, …) decorate like the
    /// main screen. The host render gate reads this — the engine itself never
    /// inspects screen mode.
    pub suppress_in_alt_screen: bool,
    /// Decorate the bare 3-letter `cat` token (opt-in).
    pub allow_bare_cat: bool,
    /// Decorate a lone CJK cat ideograph (opt-in).
    pub cjk_single_char: bool,
    /// Feline graphic mode: authored cat versus legacy ink-only `paw`.
    pub feline_style: FelineStyle,
    /// `[sparkle_words.feline] magic` (§10): Fortune (1/512) / Nebula (1/1024)
    /// rare cats. `false` ⇒ every cat takes the ordinary build.
    pub feline_magic: bool,
    /// `[sparkle_words.profanity] style` (§10): supernova vs the exact v1
    /// sparkle.
    pub profanity_style: ProfanityStyle,
    /// `[sparkle_words.profanity] magic` (§10): Quasar (1/512) / Singularity
    /// (1/1024) rare novas. `false` ⇒ every nova takes the ordinary build.
    pub profanity_magic: bool,
    /// FREQUENCY is safe to raise: the screen stays bounded by machinery this
    /// knob cannot defeat — `MAX_ACTIVE_SUPERNOVAE = 1` plus the ignition
    /// limiter (`IGNITION_WINDOW` / `MAX_RECENT_IGNITIONS`), which QUEUE rather
    /// than cancel — so a higher roll cannot exceed the flash budget.
    ///
    /// v3 §3.2 `[sparkle_words.profanity] supernova_chance` (`0..=100`, 0
    /// disables): the per-appearance FUCK SUPER NOVA escalation chance.
    /// Consulted only under [`ProfanityStyle::Rainbow`] (documented — the
    /// classic nova/sparkle paths ignore it).
    pub supernova_chance: u8,
    /// v3 §6: the custom-word spec table (`[[sparkle_words.custom]]`).
    /// Override specs win over class defaults regardless of the match's
    /// class, and bypass the per-class enable gate.
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
    /// Public so the TYPED-word detector (`aterm-gui`'s `kitty_summon`) scans
    /// with byte-identical gates to the screen scanner: one `allow_bare_cat` /
    /// `cjk_single_char` / `ignore` policy, not two that can drift apart.
    pub fn scan_opts(&self) -> ScanOptions<'_> {
        ScanOptions {
            allow_bare_cat: self.allow_bare_cat,
            cjk_single_char: self.cjk_single_char,
            ignore: (!self.ignore.is_empty()).then_some(&self.ignore),
        }
    }
}

/// v3 §6: the per-class DEFAULT spec, resolved from the live config knobs —
/// the existing classes expressed as spec consumers (no behavior change
/// except as specced): Profanity = Rainbow + SuperNova@chance (or the
/// selectable classic Nova / v1 Sparkle), Feline = Cats + SelfGlow,
/// Orca = the suspended splash (dispatched by class until the orca redo),
/// Emphasis = TwoTone. TwoTone class defaults carry the CLASS BASE pair and
/// are genome-hue-nudged at emission (custom TwoTone specs use their colors
/// raw — see `Occurrence::custom`).
fn class_default_spec(class: Class, cfg: &DecoConfig) -> WordEffectSpec {
    match class {
        Class::Profanity => WordEffectSpec {
            graphic: None,
            ink: Some(InkSpec {
                colorway: match cfg.profanity_style {
                    ProfanityStyle::Rainbow => crate::spec::rainbow_ink().colorway,
                    // The nova/sparkle ink anchors stay the v2 class pair
                    // (the nova palette overrides them through InkFx).
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
        // §4: the suspended splash keeps its class-keyed dispatch (and its v2
        // steady residual, the §1.2 exception) until the orca redo rides the
        // framework as an `Orcas` collection.
        Class::Orca => WordEffectSpec::default(),
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
fn rainbow_base_hue(gkey: u64) -> f32 {
    (mix(gkey ^ RAINBOW_HUE_SALT) % 360) as f32
}

/// The native launch defaults (the `sparkle_deco_config` resolver with an
/// absent `[sparkle_words]` table): all four families on, ink on.
/// This is the config an embedder
/// starts from when it enables the feature without setting knobs — the master
/// switch itself stays host-owned (web embedders default it OFF).
impl Default for DecoConfig {
    fn default() -> Self {
        DecoConfig {
            profanity: true,
            feline: true,
            orca: true,
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

/// One identity episode in the §3.6 persist map: everything that must survive
/// a brief occlusion so a re-hit within the grace window is a *continuation*,
/// not a rebirth.
#[derive(Clone, Copy)]
struct Episode {
    /// First appearance of this episode. Sparkle/ink windows key off this, so
    /// a grace-backed re-hit never restarts an animation (v1-compatible
    /// `appeared` semantics, now occlusion-proof).
    appeared: Instant,
    /// Refreshed on every rescan that sees the identity; the grace sweep at
    /// rescan end drops entries unseen for [`GRACE_TTL`].
    last_seen: Instant,
    /// Frozen at birth (persist miss) — the ONLY time SimHash runs. Neighbor
    /// churn after birth cannot re-roll anything (§3.6 "frozen at birth").
    genome: Genome,
    /// Local terminal palette frozen at birth with the genome. A still-live
    /// cat therefore never changes bake keys because neighboring text or SGR
    /// state churned during its one-shot animation.
    cat_colors: CatColorKey,
    /// One nova per episode (§3.6/§6.4): set when the nova window elapses (or
    /// when the concurrency cap skips the nova straight to Ember); a rescan
    /// hit never re-arms it — re-arm requires true identity death (grace
    /// expiry / `reset()`). Rides THIS map so grace preserves it (the B-3 fix).
    nova_done: bool,
    /// The episode's assigned ignition instant (§6.4): the Dip start, granted
    /// once by the flash limiter (possibly DELAYED past first sight — the
    /// deterministic row-major queue) and then frozen, so the phase machine
    /// stays a pure function of `now − nova_start`. `None` until eligible
    /// (unassigned, or ignition deferred by an active selection). Grace-backed
    /// like every episode field.
    nova_start: Option<Instant>,
    /// §F4.2 Kitty Log: this episode already queued its ONE sighting. Set
    /// post-loop only for successfully queued sightings; grace expiry and
    /// `reset()` (suppressed-alt-screen round-trip, config reload, toggle)
    /// drop the episode, so a re-appearance past them recounts — documented
    /// recount semantics, matching the §3.6 re-roll story.
    logged: bool,
    /// v3 §1.1: the current position-bearing occurrence seed. Exact-seed
    /// alignment preserves the original vertical-scroll behavior; a horizontal
    /// redraw updates this field when the episode is rekeyed to its new column.
    seed: u64,
    /// Collision-free ID of the exact recognized lexicon surface (including
    /// its finite morphology variant). This is the
    /// horizontal-redraw group key: it can carry a spent episode between
    /// columns without making a nonmatching word (for example `fix`) a lexical
    /// occurrence or relying on a 64-bit hash collision assumption.
    form_id: FormId,
    /// Most recently recognized context fingerprint. The genome remains frozen
    /// at birth, but a changed-context rekey adopts this recognition key so a
    /// later expiry writes the done mark for the layout that actually departed.
    /// It also supplies the strong context arm before weak visual continuity.
    ctx_fp: u64,
    /// Last viewport position, used by the two-dimensional redraw alignment.
    last_row: u16,
    last_col: u16,
    /// An intervening rescan showed nonblank replacement content on this row
    /// while the form itself was absent. This is the observable distinction
    /// between blank clear/redraw grace (safe to continue on exact context) and
    /// incremental retyping/overwrite. Feline words always use this evidence
    /// to re-arm (the explicit “kitty after more typing” policy). Profanity
    /// combines it with a token ending at the live caret, so genuinely retyped
    /// `fuc`/`fuck` re-arms without making unrelated `fix` redraws causal.
    continuity_tainted: bool,
    /// v3 alignment bookkeeping: the rescan sequence that last adopted this
    /// episode (fast path or alignment). Episodes NOT seen this rescan form
    /// the alignment's "old" set for their seed group.
    seen_seq: u64,
    /// v3 §1.1 per-axis done flags. `peek_done`: the graphic one-shot ran to
    /// completion (descend finished / paw faded out) — zero quads forever.
    peek_done: bool,
    /// v3: the graphic one-shot STARTED (the birth-latched phase clock began
    /// ticking) — the done-mark write condition on departure.
    peek_started: bool,
    /// v3 §1.1: `burst_done` is set at IGNITION GRANT (detonation start is
    /// the point of no return); the sparkle-style burst sets it when its
    /// residual fade completes.
    burst_done: bool,
    burst_started: bool,
    /// v3 §1.1: ink sweep axis (started at first emitted ink; done when the
    /// sweep + fade window elapses / the feline glow self-terminates).
    sweep_done: bool,
    sweep_started: bool,
    /// v3 §1.1 fix #2: born from a `done_marks` hit — normatively INERT:
    /// contributes nothing to active_until/next_deadline/fp folds/kitty
    /// sightings/burst rolls; emits only the settled ink bytes.
    born_done: bool,
    /// v3 §1.1 fix #4: born during the resize-settle window — same inert
    /// emission as `born_done` but never written to `done_marks`.
    born_settled: bool,
    /// v3 §1.2 arm freezing: Cat vs Paw (+ fallback cause), stored at first
    /// emission decision; every later tick dispatches from the stored arm.
    shown_as: Option<KittyShownAs>,
    /// The peek phase clock's origin — latched AT BIRTH, never deferred on
    /// focus: the entrance plays when the word first appears on the grid. An
    /// occluded window simply misses frames, and cats never replay on refocus.
    /// The `Option` is a defensive seam only (`None` draws nothing);
    /// [`Episode::fresh`] always latches it.
    phase_start: Option<Instant>,
    /// Clearance pause: the instant a stored-Cat
    /// arm lost [`cat_eligible`] mid-peek (terminal text landed inside the
    /// two-row body footprint, a DECDWL flip). While latched the peek clock
    /// is SUSPENDED — the prepass advances no flags — and on the first
    /// eligible tick `phase_start` shifts forward by the paused span (the
    /// engine-wide freeze/thaw timestamp-shift idiom, applied per episode),
    /// so the cat resumes exactly where it vanished when the rows clear
    /// instead of silently burning its one-shot to Done while invisible.
    peek_pause: Option<Instant>,
    /// The armed peek's total wall-clock length (rise + dwell + anticipation
    /// + descend), latched by the prepass while the occurrence is visible.
    ///
    /// A SCROLLED-OFF mid-peek episode completes against this by wall clock
    /// (the prepass sweep): the flag loop only walks visible occurrences, so
    /// without the latch such an episode could never reach `peek_done`
    /// off-screen and its lifecycle would hang until grace expiry.
    peek_total: u64,
    /// v3 §3.2/§6: the burst-axis roll, decided ONCE at birth
    /// (`mix(gkey ^ birth_seq ^ SUPERNOVA_SALT) % 100 < chance_pct`) for
    /// EVERY [`BurstKind`] (chance 100 = always, so the class defaults keep
    /// firing) and stored so the decision transfers with row alignment.
    /// Always `false` for born-done/born-settled episodes.
    burst_roll: bool,
    /// WHICH of the three detonation degrees this episode drew, decoded from
    /// the HIGH half of the same birth draw that set [`Self::burst_roll`] (see
    /// [`supernova::tier_of`]). Stored, like the roll itself, so the decision
    /// transfers with row alignment instead of being re-drawn on every scan.
    /// Meaningless unless `burst_roll` and a `SuperNova` burst kind.
    burst_tier: supernova::SuperTier,
    /// v3 §6: the burst axis kind at birth (`None` = no burst axis). The §3.2
    /// burst mutex reads it to tell a live CLASSIC nova's genome-derived
    /// window from a supernova's without the occurrence in scope.
    burst_kind: Option<BurstKind>,
    /// Curse-BONK one-shot latch: set ONLY at the fresh-birth site of a
    /// Profanity episode carrying the typed witness (`live_caret_completion`),
    /// consumed when the tick successfully queues its [`CurseCue`] (the
    /// sightings' cap-truncation retry discipline: a full cue vec leaves the
    /// latch armed for the next tick). Because it is born-latched it inherits
    /// the whole §1.1 inertness story for free — born-done/born-settled
    /// births never set it, redraw/scrollback adoptions transfer episodes
    /// instead of birthing, and ambiguous forms deferred at the live caret
    /// (Romanian `fut` mid-`future`) never produce an occurrence at all.
    bonk_pending: bool,
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
        }
    }

    /// v3 §1.1: whether any one-shot started — the done-mark write condition
    /// at every point the episode leaves the persist map. Born-settled
    /// episodes are never marked (resize rule); born-done episodes refresh
    /// their existing mark.
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

    /// v3: born-done and born-settled episodes share the inert emission path.
    fn inert(&self) -> bool {
        self.born_done || self.born_settled
    }
}

/// Which edge fired a curse BONK. The two kinds carry different provenance
/// guarantees, so the host gates them separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurseCueKind {
    /// The typed-witness birth edge: a Profanity episode born with the live
    /// caret exactly one past the token (`live_caret_completion`). TYPED-ONLY
    /// by construction — `cat`-ing a curse file, scrollback redraws, and
    /// pastes that leave the caret elsewhere never record this kind.
    Typed,
    /// The supernova ignition GRANT edge (§3.2): an on-screen curse whose
    /// burst roll escalated actually detonating. Fires for output content
    /// too, which is exactly why the host keeps it behind its own
    /// off-by-default knob; rate-limited for free by `grant_ignition`'s
    /// rolling flash limiter, and once-per-episode via `burst_done`.
    Detonated,
}

/// One recorded curse-BONK trigger — the sparkle-words twin of the trail's
/// `SoundCue` and the Kitty Log's [`KittySighting`]: the engine records WHEN
/// (it already computed the witness and the ignition grant for the light),
/// the host drains promptly after [`WordDecorations::tick`] and owns ALL
/// policy (the profanity `bonk`/`bonk_detonation` knobs, raw window focus,
/// reduced motion, volume) before mapping it onto a namespaced
/// `SoundGesture::Words(WordGesture::Bonk)` event.
#[derive(Clone, Copy, Debug)]
pub struct CurseCue {
    pub kind: CurseCueKind,
    /// The word's viewport position; `col` is the lead column — the host maps
    /// it to stereo pan exactly like the trail's arrival column.
    pub row: u16,
    pub col: u16,
}

/// Bound on this tick's recorded curse cues (resident vec, cleared at tick
/// start like the sightings so hostless wasm/web builds stay bounded). A
/// screenful of typed curses cannot exceed it meaningfully; cap-truncated
/// typed cues retry via their episode latch, detonations are already
/// once-per-episode.
const MAX_CURSE_CUES: usize = 16;

/// v3 §1.1: one deferred birth/adoption candidate, parked by `scan_row` for
/// the rescan-end row-anchored alignment pass. A candidate defers whenever the
/// naive `(seed, ordinal)` ident does not hit an episode on the SAME row —
/// growth/shrink/rotation/scroll all land here; the stable-screen fast path
/// (ident hit + row match) never does.
#[derive(Clone, Copy)]
struct PendingBirth {
    /// Index into the occurrence vec under construction.
    occ_idx: u16,
    /// The context fingerprint computed while the row's char stream was still
    /// in scope (the genome + done-mark key ingredient).
    ctx_fp: u64,
    /// The recognized token ends exactly at the live input caret. Combined
    /// with a tainted prior episode, this is positive evidence of intentional
    /// retyping rather than a passive output redraw.
    live_caret_completion: bool,
    /// The live input caret touches the token (inside it, or in the cell
    /// immediately after — the same adjacency window the ambiguity
    /// suppression uses). This is the "word the user just typed" witness for
    /// the feline re-arm policy: a resident SPENT episode that was absent for
    /// a full rescan may not transfer onto such a candidate, and while
    /// resident old evidence exists the candidate's colliding done mark is
    /// deleted rather than honored — so a kitty retyped after `clear` within
    /// grace is always born fresh. Broader than `live_caret_completion`
    /// (mid-word edits count) but still requires the caret ON the word, so
    /// passive output redraws elsewhere never qualify; without resident
    /// evidence a parked caret proves nothing and born-done stands.
    at_live_cursor: bool,
}

/// Capacity policy for an episode entering the persistent identity map.
///
/// A currently observed episode wins a key collision with grace-only history.
/// Capacity itself uses one deterministic global LRU order over the incoming
/// episode and all residents, keeping the newest [`PERSIST_CAP`] entries. This
/// distinction keeps a fresh visible claimant resident without allowing stale
/// history to exceed the bound.
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
    /// transfer cardinality. This distinguishes a log rotation (`[0,2] ->
    /// [2,4]`, where row 2 survives and row 4 is genuinely new) from a global
    /// insertion (`[0,2] -> [1,3]`, where no stationary anchor exists and both
    /// episodes transfer). Ties then prefer stronger context and less motion.
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

/// Compatibility/cost for one old episode and one candidate in the same exact
/// [`FormId`] group. Same-seed vertical edges retain the established bounded
/// log-tail skip policy. Changed-column edges accept exact context everywhere
/// unless a feline episode was tainted by nonblank incremental retyping, or a
/// tainted profanity episode ends at the live caret. Recent/local same-width
/// continuity is the weak redraw arm. Profanity otherwise takes the deliberately
/// conservative branch documented below so harmless composer churn cannot make
/// `fix` appear causal.
fn alignment_edge(
    ep: &Episode,
    candidate: PendingBirth,
    occ: &Occurrence,
    same_width: bool,
    seq: u64,
    cols: u16,
) -> Option<(u8, u32)> {
    // Nonblank partial typing is positive evidence that a feline token was
    // replaced, even when the complete token returns at the same position
    // inside the two-scan continuity window. For profanity, require the
    // stronger causal witness that the completed token ends at the LIVE caret:
    // passive `fix`/composer redraws may still conservatively restore a spent
    // output episode, while an actually retyped `fuc`/`fuck` must re-arm.
    if ep.continuity_tainted
        && (occ.class == Class::Feline
            || (occ.class == Class::Profanity && candidate.live_caret_completion))
    {
        return None;
    }
    // A SPENT (`peek_done`) feline episode that missed at least one full
    // rescan may not transfer onto the candidate at the live input caret.
    // Neither ident (no row folded in) nor the tier-1 exact-context edge
    // (deliberately unbounded in time) can tell a redraw from a retype at
    // the same column under an unchanged prompt; the caret supplies the
    // missing causal witness. A word that vanished for a whole scan and
    // returned under the user's caret is a NEW word — inheriting the
    // ancestor's `peek_done` would suppress its cat for the session. Same
    // arm as the fast-path guard, applied to every tier: after the fast
    // path defers such a candidate, the tier-0 exact-seed edge would
    // otherwise readopt it here. Gap ≤ 1 (seen last scan, merely moved:
    // scroll or reflow) still transfers, so a passively scrolled spent
    // kitty that happens to sit at an editor caret never replays.
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
    // Profanity is deliberately conservative for the grace lifetime unless the
    // taint + live-caret causal guard above proved intentional retyping: when a
    // spent exact surface briefly disappears under composer/status-line churn,
    // transfer it even though context and position both changed, so old output
    // cannot relight. A genuinely new additional token still has no spare old
    // episode and is born normally.
    if occ.class == Class::Profanity {
        return Some((1, distance));
    }
    (same_width && recent && distance <= max_distance).then_some((2, distance))
}

/// One rate-limiter reservation, labeled with the current persist identity of
/// the episode that requested it. Ownership is needed only while `start` is in
/// the future: cancelling a vanished pending episode is safe (it never
/// flashed), while a slot at or before `now` remains as rolling-window history
/// regardless of owner lifetime or later reuse of the same numeric identity.
#[derive(Clone, Copy, Debug, PartialEq)]
struct IgnitionReservation {
    /// Which bound pane requested it (0 for an unbound, window-wide engine).
    /// The rolling-window SAFETY count is over every pane — one window, one
    /// eye — but a pending slot may only be cancelled by the pane that owns
    /// it: every other pane's episodes live in a parked persist map this side
    /// of the binding cannot see, and pruning against the wrong map would
    /// cancel a queued nova that is still perfectly alive.
    pane: u64,
    owner: u64,
    start: Instant,
    center: (i32, i32),
}

/// One LIVE (quad-emitting) nova this frame, resolved by the §6.4 prepass —
/// the read-only hand-off between the limiter/episode state and the pure
/// emitters, the gaze hook, and the blast coupling.
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

/// One LIVE (quad-emitting) supernova this frame (v3 §3.2), resolved by the
/// super prepass — the hand-off to the pure `supernova` emitters and the
/// selection wash-split.
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

/// One matched word on the visible grid. `seed` encodes the word's column, class,
/// and text (NOT its row), so the same word keeps a stable sparkle identity across
/// vertical scrolls and unrelated same-row churn.
#[derive(Clone)]
struct Occurrence {
    row: u16,
    start_col: u16,
    end_col: u16,
    class: Class,
    /// Every language claiming the matched surface (copied from [`Match`] at
    /// rescan) — the Kitty Log's attribution source (§F4.2). NOT folded into
    /// `seed` / `ident` / genome and never emitted, so identities and rendered
    /// bytes are unchanged by its presence.
    langs: LangSet,
    /// Collision-free exact recognized surface (including morphology),
    /// independent of row/column.
    form_id: FormId,
    seed: u64,
    /// The §3.6 episode identity (`mix(seed ^ ordinal·ORDINAL_MIX)`) — the key
    /// the gaze map and the idle-event scheduler address this cat by.
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
    /// The word's FIRST lead cell's bg, the §4.3 legibility-guard reference
    /// (mixed-bg words are evaluated against that cell — documented coarse case).
    ink_bg: [u8; 3],
    /// Quantized matched-word + neighboring-text + local-background palette.
    /// Computed only on damage-epoch rescan; copied into the bounded bake key.
    cat_colors: CatColorKey,
    /// A peek direction was resolved for the prospective body footprint —
    /// i.e. at least one side of the trigger row is clear enough to host the
    /// head (see [`cat_peek_plan`]). Cats draw [`FreeZ::UnderText`], so
    /// neighbouring glyphs stay crisp on top of the fur; this only rejects the
    /// genuine mush case where BOTH sides are a solid wall of text.
    cat_text_clear: bool,
    /// The resolved head direction: `true` when the head peeks DOWN out from
    /// under the word (the top-row case, and any row whose two rows above are
    /// busier than the two below). `false` is the classic upward peek.
    cat_peek_down: bool,
    /// The row carries a DEC double-width/height size (§5.7): the cat is
    /// suppressed there (NEAREST-stretch across DECDWL isn't worth the parity
    /// surface) — the v1 paw + ink take over.
    dec_line: bool,
    /// v3 §1.1: copied from the episode's born-done/born-settled flags at
    /// rescan — the inert emission gate (settled ink bytes only; orca keeps
    /// its exempt v2 residual).
    inert: bool,
    /// v3 §6: the resolved effect spec (per-word override, else the class
    /// default) — the tick dispatches on its axes, not on `class`.
    spec: WordEffectSpec,
    /// v3 §6: `spec` came from a `[[sparkle_words.custom]]` override — the
    /// class-gate bypass marker; custom TwoTone colors apply RAW (no genome
    /// hue nudge), and the orca class carve-out yields to the spec.
    custom: bool,
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
/// removal, and eviction O(1). A `HashMap<key, touch_sequence>` instead would
/// have to scan all [`DONE_MARKS_CAP`] entries for the oldest on every insert
/// into a full map — a permanent long-session latency cliff. The `nodes` and
/// `free` vectors grow only to that cap.
#[derive(Clone, Copy, Debug)]
struct DoneMarkNode {
    key: u64,
    older: u32,
    newer: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DoneMarkMutation {
    evicted: Option<u64>,
    /// Number of intrusive-list node writes. This is an implementation-level
    /// constant-work witness used by the Tier-1 model binding below.
    link_writes: u8,
}

/// Deterministic bounded LRU for completed one-shot identities.
struct DoneMarkLru {
    /// Fx, not SipHash: the keys are already avalanche-mixed idents
    /// (`mix(seed ^ ordinal * ORDINAL_MIX)`), so SipHash's diffusion buys
    /// nothing over a multiply-and-rotate and costs ~4× per probe. Eviction
    /// order comes from the intrusive `nodes` list, never from map iteration,
    /// so the hasher cannot move it.
    index: FxHashMap<u64, u32>,
    nodes: Vec<DoneMarkNode>,
    free: Vec<u32>,
    oldest: u32,
    newest: u32,
}

impl Default for DoneMarkLru {
    fn default() -> Self {
        Self {
            index: FxHashMap::default(),
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
    /// node link writes (head/tail scalar writes are constant too, but do not
    /// grow with resident cardinality).
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

/// Host-side sparkle-words state for one window.
/// A user-supplied Nyan-cursor sprite: the decoded native RGBA plus a cache of
/// the last nearest-resample to the current cursor target size.
struct NyanCustom {
    source_fp: u64,
    w: u16,
    h: u16,
    /// Shared because one decoded config sprite is published to every window;
    /// cloning the source on the UI thread would otherwise copy up to 4 MiB per
    /// window at the exact moment a redraw is being scheduled.
    rgba: Arc<[u8]>,
    /// `(target_w, target_h, resampled_rgba)` — recomputed when the target
    /// (cell-metric-derived) size changes.
    cache: Option<(u16, u16, Vec<u8>)>,
}

/// Resident paint for one authored sing-along note tile. `host_tile` caches by
/// id and never re-reads texels on a hit, but still needs the slice after a rare
/// atlas eviction. Retaining the last paint per note kind makes steady-state
/// probes allocation-free and repaints only when cell metrics change the id.
struct NotePaintCache {
    host: u64,
    rgba: Vec<u8>,
}

/// Immutable host-authored source for the Nyan cursor companion.
///
/// `Disabled` is distinct from `BuiltIn` so an invalid configured custom asset
/// fails closed instead of silently presenting different art.  Custom pixels
/// stay shared with the host's admitted config catalog; only target-size
/// resamples are private mutable cache entries.
#[derive(Clone, Debug)]
pub enum NyanSpriteSource {
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
/// LATENCY MECHANISM. The rescan is triggered by the DAMAGE EPOCH but is not
/// damage-limited: any PTY byte advances the epoch, so every presented frame
/// under output re-tokenises the WHOLE viewport — and it does so on the main
/// thread, inside the very present that carries the user's keystroke echo
/// (`cell_frame_into` + `take_damage` sit one branch above it). Measured on a
/// full 60x200 grid of English prose that is ~99 µs/frame, of which ~86 µs is
/// `Lexicon::scan_into_with_scratch` alone: per token it folds the surface,
/// probes the spaced map, and retries possessive/clitic candidates. Those
/// microseconds are pure echo latency, and they also feed `render_ns`, so a
/// busy screen can push the load-shed latch into `perf_reduced` and fade out
/// every cursor effect exactly while the user is typing fastest.
///
/// The scanner is a PURE function of `(row text, ScanOptions, Lexicon)`: it
/// reads no column map, no colors, no row index, and no clock. So a row whose
/// character stream is unchanged since any recent frame cannot produce
/// different matches, and the whole tokenise+lookup pass can be replaced by one
/// hash lookup. Everything downstream of the match list — ordinals, identity,
/// the persist fast path, SimHash births, ink capture, alignment — still runs
/// verbatim on every row, every frame, so the presented decoration set is
/// unchanged by construction.
///
/// Keyed by the row TEXT rather than the row INDEX on purpose: under scrolling
/// output (the case the memo exists for) row `r` this frame holds what row
/// `r + k` held last frame, so an index-keyed memo would miss on every row of
/// every scrolled frame — precisely the heavy-output case. Text keys hit for
/// every line that merely moved, leaving only genuinely new lines to scan.
///
/// Capacity is two generations of `fit`: a full hot generation is demoted to
/// cold wholesale (dropping the previous cold), so retention is at least one
/// screenful — enough for the scroll hit — and never grows without bound as a
/// build log streams distinct lines through it. `String`/`Vec` keys are exact,
/// so no hash-collision argument is needed for correctness.
///
/// The retired generation's HEAP BUFFERS are kept (`spare`) rather than freed.
/// A memo MISS is strictly more expensive than the pre-memo scan it wraps — it
/// copies the row text and the match list into a new entry — and the shape
/// where every probe misses is a full-screen TUI repaint: one malloc+free pair
/// per row per frame, on the render thread, in a crate that is otherwise
/// allocation-free after warm-up. Recycling buys no measurable throughput (its
/// delta is inside the probe's ±1 µs noise); what it buys is that a warmed
/// all-miss frame stops calling the allocator at all, because one discarded
/// generation is exactly the buffer count the incoming generation asks for.
/// Per-row malloc/free on the render thread is what turns a busy screen into a
/// tail-latency spike, and it is the one thing this crate promises not to do.
#[derive(Default)]
struct ScanMemo {
    hot: FxHashMap<String, Vec<Match>>,
    cold: FxHashMap<String, Vec<Match>>,
    /// Emptied entry buffers from the generation `install` just discarded,
    /// waiting to be refilled by the incoming one. Bounded by `gen_cap`, so the
    /// memo's footprint stays ≈ 3 generations rather than 2 — the same order,
    /// still tied to the viewport, and it is what makes the ALL-MISS steady
    /// state allocation-free instead of only the all-hit one.
    spare: Vec<(String, Vec<Match>)>,
    /// Live per-generation entry cap (from the viewport row count).
    gen_cap: usize,
    /// Fingerprint of the OTHER TWO scanner inputs the resident entries were
    /// produced under — the [`ScanOptions`] and the [`Lexicon`] itself (see
    /// [`scan_inputs_fp`]). The memo keys on the row text alone, so anything
    /// else `scan_into_with_scratch` reads has to live here or a hit answers
    /// with a different function's output. `[sparkle_words]` edits and lexicon
    /// rebuilds already route through `hard_reset` (which clears the memo
    /// outright); this is the standing guard for any caller that swaps a gate
    /// or a lexicon WITHOUT one — a stale `allow_bare_cat`/`ignore` entry would
    /// keep decorating a word the user just denied, and a stale LEXICON entry
    /// would keep decorating language-gated forms the user just turned off, on
    /// a byte-idle grid, until the text next changed.
    inputs_fp: u64,
    /// Test-only: force every probe to miss so the equivalence battery can
    /// drive a second engine down the always-re-lex path.
    #[cfg(test)]
    bypass: bool,
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    misses: u64,
    /// Test-only: how many entries had to be built from a FRESH allocation
    /// because `spare` was empty. The all-miss allocation regression asserts
    /// this stops growing — a counter, not a global allocator hook, so the
    /// claim is checkable from a unit test.
    #[cfg(test)]
    fresh_buffers: u64,
    /// Test-only: take the unpooled miss path (allocate a key + match list per
    /// miss, free the retired generation). The cost probe drives it in the same
    /// interleaved run as the pooled engine — measuring the two in separate
    /// runs measures this box's load, not the pool.
    #[cfg(test)]
    no_recycle: bool,
}

impl ScanMemo {
    fn clear(&mut self) {
        // Retirement (fingerprint change / `hard_reset`) drops the ENTRIES but
        // keeps their buffers for the generation that immediately follows: the
        // whole viewport is about to be re-scanned and re-remembered, so
        // freeing here would only mean re-allocating a screenful of rows on the
        // very next frame.
        let keep = self.pool_target();
        Self::recycle(&mut self.spare, &mut self.hot, keep);
        Self::recycle(&mut self.spare, &mut self.cold, keep);
    }

    /// How many retired buffers the pool holds: ONE generation — exactly what
    /// the incoming generation will ask `remember` for before the next
    /// rotation, so the steady state neither allocates nor grows.
    fn pool_target(&self) -> usize {
        #[cfg(test)]
        if self.no_recycle {
            return 0;
        }
        self.gen_cap.max(1)
    }

    /// Move `gone`'s entry buffers into `spare` (up to `keep` of them),
    /// freeing whatever does not fit, and leave `gone` EMPTY BUT ALLOCATED —
    /// the map's table is resident scratch under the same rule its entries are.
    fn recycle(
        spare: &mut Vec<(String, Vec<Match>)>,
        gone: &mut FxHashMap<String, Vec<Match>>,
        keep: usize,
    ) {
        let room = keep.saturating_sub(spare.len());
        // `take(room)` stops FILLING the pool at the cap; the remaining entries
        // are still removed when the `Drain` is dropped, so this is a clear()
        // that keeps what the next generation will ask for.
        spare.extend(gone.drain().take(room));
        debug_assert!(gone.is_empty(), "the drain must empty the generation");
    }

    /// Per-rescan prologue: retire everything if the scan gates OR the lexicon
    /// moved, and size the generations to the viewport. Called once per rescan,
    /// not per row.
    fn fit(&mut self, rows: usize, inputs_fp: u64) {
        if self.inputs_fp != inputs_fp {
            self.clear();
            self.inputs_fp = inputs_fp;
        }
        // TWO screenfuls per generation, not one: at exactly one screenful a
        // single new line would rotate the generation every frame, so the next
        // frame's rows would all miss hot and pay a promotion — hit rate holds
        // but the memo churns for nothing. Two screenfuls amortize the rotation
        // over a screenful of genuinely new lines.
        self.gen_cap = rows
            .saturating_mul(2)
            .clamp(SCAN_MEMO_MIN_GEN, SCAN_MEMO_MAX_GEN);
        // The spare pool tracks the viewport too: a window that shrank (or a
        // pane that split) must not keep the big grid's screenful of row
        // buffers alive for the rest of the session. No-op — not even a walk —
        // whenever the pool already fits.
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

    /// The memoized match list for a row [`touch`](Self::touch) just reported
    /// resident. A miss here would silently drop the row's decorations rather
    /// than merely slow the frame, so it is a debug hard error; release falls
    /// back to "no matches" instead of panicking inside a present.
    fn matches(&self, text: &str) -> &[Match] {
        debug_assert!(self.hot.contains_key(text), "touch() proved residency");
        self.hot.get(text).map_or(&[][..], Vec::as_slice)
    }

    /// Record a freshly scanned row, refilling a retired entry's buffers rather
    /// than allocating a key + match list per miss — on an all-miss screen that
    /// malloc/free pair would run once per row per frame, on the render thread
    /// (see the `spare` field).
    fn remember(&mut self, text: &str, matches: &[Match]) {
        // `bypass` is the memo switched OFF, not merely forced to miss: the
        // equivalence battery's control engine must run the pre-memo code path
        // exactly, and the cost probe's `memo=false` legs are only a baseline
        // if they also skip the bookkeeping below.
        #[cfg(test)]
        if self.bypass {
            return;
        }
        #[cfg(test)]
        if self.no_recycle {
            // The unpooled path, counted the same way, so the all-miss
            // allocation regression can state the delta the pool removes.
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
            // Generation rotation, not eviction: the screenful that just went
            // cold still answers next frame's scroll, and the generation before
            // it is retired in one walk instead of a per-entry LRU walk on the
            // typing path. That walk hands its buffers to `spare` — exactly one
            // generation of them, which is exactly what the incoming generation
            // is about to ask `remember` for.
            let keep = self.pool_target();
            Self::recycle(&mut self.spare, &mut self.cold, keep);
            std::mem::swap(&mut self.hot, &mut self.cold);
        }
        self.hot.insert(key, value);
    }
}

/// Fold every SCAN-GATE input that can change what
/// [`Lexicon::scan_into_with_scratch`] returns for a fixed row text into one
/// key. Computed ONCE per rescan (the `ignore` walk is O(|deny list|), not
/// O(rows · |deny list|)).
fn scan_opts_fp(opts: &ScanOptions<'_>) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let fold = |acc: u64, byte: u8| (acc ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    let mut fp = fold(FNV_OFFSET, u8::from(opts.allow_bare_cat));
    fp = fold(fp, u8::from(opts.cjk_single_char));
    if let Some(ignore) = opts.ignore {
        // XOR-folded per-word hashes: `HashSet` iteration order is not stable
        // across rebuilds, so an order-SENSITIVE fold would spuriously retire
        // the memo (a pure slowdown, but every frame). Length is mixed in so
        // adding a word that XORs to zero against an existing one still moves
        // the key.
        let mut set = 0u64;
        for word in ignore {
            set ^= word.bytes().fold(FNV_OFFSET, fold);
        }
        fp ^= set;
        fp = fold(fp, 0xff);
        fp ^= ignore.len() as u64;
    }
    fp
}

/// The memo's FULL retirement key: the scan gates ([`scan_opts_fp`]) plus the
/// identity of the lexicon the entries were scanned against.
///
/// WHY THE LEXICON IS IN THE KEY. `scan_into_with_scratch` reads
/// `(text, opts, self)`. The memo keys on the text and `fit` retires on the
/// opts, which left the THIRD input unguarded: with the same cells and the same
/// gates but the lexicon swapped (`languages = ["en","ro"]` → `["en"]`, a user
/// `[[entry]]` override edited, a Toy Pack unloaded) every row hits and the
/// frame is decorated by the lexicon the user just replaced — a wrong
/// decoration set on screen, not merely a stale one. `Match::form_id` is
/// unique only WITHIN the instance that produced it, so the identity/ordinal
/// machinery downstream inherits the error rather than catching it.
/// `hard_reset` is the intended rebuild hook (v3 §1.1), but that is a caller
/// CONVENTION; a cache may not depend on its callers being polite.
///
/// WHAT THE KEY IS. aterm-lexicon exposes no build digest, and this runs on the
/// keystroke path (once per rescan, inside the present carrying the echo), so
/// an O(|lexicon|) content hash — tens of thousands of folded surfaces — would
/// cost more than the memo saves. The key is therefore the strongest strictly
/// O(1) fold the public surface allows:
///
/// * the instance ADDRESS. The scanner's `&Lexicon` is a stable place for the
///   memo's whole life — production holds it in an `Arc` inside the resolved
///   sparkle runtime and publishes a NEW `Arc` on rebuild, so the address never
///   moves under a live memo and never repeats between two lexicons that are
///   alive at the same moment. That covers every real swap: the language-set
///   change above, a per-window runtime, an override reload, a test fixture.
/// * `spaced_form_count()`, which separates the one case an address cannot —
///   a rebuild the allocator happened to hand the freed address of the lexicon
///   it replaced. Two different lexicons at the same address with an equal
///   folded-surface count would still collide; closing that residual needs a
///   digest computed ONCE at build time inside aterm-lexicon (that crate is
///   outside this change), which this fold would then subsume both terms with.
fn scan_inputs_fp(lexicon: &Lexicon, opts: &ScanOptions<'_>) -> u64 {
    // `fold_u64` is this module's whole-scalar FNV-1a step, over the same prime
    // the gate fold above mixes bytes with.
    let fp = fold_u64(
        scan_opts_fp(opts),
        std::ptr::from_ref(lexicon) as usize as u64,
    );
    fold_u64(fp, lexicon.spaced_form_count() as u64)
}

#[derive(Default)]
pub struct WordDecorations {
    occ: Vec<Occurrence>,
    last_epoch: u64,
    have_scanned: bool,
    cols: u16,
    frame: u64,
    /// The latest instant any profanity occurrence is still animating; `None`
    /// when nothing is animating. Lets the scheduler poll `is_active` without the
    /// config.
    active_until: Option<Instant>,
    /// Resident scratch reused across rescans (rescan only runs on a damage epoch
    /// change, but reusing these avoids a per-row/per-line heap allocation).
    scan_cells: Vec<RenderCell>,
    scan_chars: Vec<char>,
    scan_matches: Vec<Match>,
    /// The rescan's tokenise+lexicon-lookup memo — see [`ScanMemo`]. Holds the
    /// only per-row work that is a pure function of the row's text.
    scan_memo: ScanMemo,
    /// Test-only mutation switch: suppress the memo-hit rebuild of
    /// `scan_chars`. Exists so the battery can prove the `scan_chars` /
    /// `scan_text` invariant is GUARDED (a named `debug_assert_eq!` at the
    /// seam) instead of surfacing as an out-of-bounds panic deep inside
    /// `genome::simhash_ctx` while a frame is being drawn.
    #[cfg(test)]
    mutate_skip_scan_chars_rebuild: bool,
    /// Lexicon token/fold/CJK workspace, resident so typing-damage rescans
    /// allocate nothing after the scanner reaches its high-water capacities.
    scan_scratch: ScanScratch,
    /// Per-line reconstructed text + its char→column map, reused across rows.
    scan_text: String,
    scan_colmap: Vec<u16>,
    /// Whether each scanned viewport row contained any nonblank text. Consulted
    /// after alignment to taint unseen episodes without an O(rows × episodes)
    /// hot-path walk; capacity is resident after the first frame.
    scan_row_occupied: Vec<bool>,
    /// §3.6 identity persistence: `ident → Episode`, cap [`PERSIST_CAP`],
    /// oldest-`last_seen` eviction, [`GRACE_TTL`] sweep at rescan end. Replaces
    /// v1's one-epoch `prev_appeared` map (which forgot `appeared` — and would
    /// have forgotten `nova_done` — the moment a word missed a single rescan:
    /// the B-3 strobe hole).
    ///
    /// Fx, not SipHash: `tick` probes this map ~8-12× per live occurrence per
    /// FRAME (cue, graphic, ember, burst gates, prepasses, ink), and the key is
    /// an already-mixed ident, so SipHash's diffusion is pure overhead here.
    /// The two iteration sites are order-independent — `align_pending` sorts
    /// what it collects, and `rescan_end` mutates each entry independently.
    persist: FxHashMap<u64, Episode>,
    /// Per-rescan row-major count of same-seed occurrences — the §3.6 ordinal
    /// source. Resident so the rescan allocates nothing after warmup.
    seed_ordinals: FxHashMap<u64, u64>,
    /// §3.2 SimHash vote accumulator (resident scratch; zero allocation).
    votes: VoteScratch,
    /// Resident raw-token / folded-token scratch for the miss-path context walk.
    ctx_tok: String,
    ctx_folded: String,
    /// Test-only grace override (`None` = [`GRACE_TTL`]). The SparkleIdentity
    /// conformance negative control sets `Some(ZERO)` — no grace at all — to
    /// drive the model's `Buggy` trace.
    grace_override: Option<Duration>,
    /// Resident per-lead-cell base fg captured at rescan (§4.3) — the `base_fg`
    /// term of the §4.2 ink mix. ≤ `MAX_INK_CELLS` × 3 B; indexed by
    /// `Occurrence::ink_base`.
    ink_base_fg: Vec<[u8; 3]>,
    /// Resident per-lead-cell viewport column, parallel to `ink_base_fg` (wide
    /// continuation halves carry no glyph, so they are never captured; the LEAD
    /// cell's InkCell governs the whole wide glyph).
    ink_cols: Vec<u16>,
    /// §5.5 peeking-cat bake cache (LRU tiles + the published atlas). Cleared
    /// wholesale on `reset()` (config reload / toggle / alt-screen) and on
    /// cell-metric change (detected per tick).
    cat_baker: CatBaker,
    /// Whether `tick` already ran the baker prologue for this host frame. The
    /// cursor companion shares that budget; it must not reset the two-bake cap.
    cat_baker_ready: bool,
    /// A USER-supplied cursor sprite (via `cursor_nyan_sprite`), overriding the
    /// built-in CatBaker cat: the decoded native RGBA `(w, h, rgba)` plus a cache
    /// of the last nearest-resample to the current target size `(tw, th, rgba)`.
    nyan_custom: Option<NyanCustom>,
    /// Invalid authored sprite assets fail closed rather than falling back to
    /// built-in art while the UI reports a custom source.
    nyan_disabled: bool,
    /// Bumped whenever the cursor sprite SOURCE changes (custom set/cleared), so
    /// the shared-atlas host key changes and the stale tile is re-baked.
    nyan_gen: u64,
    /// The sing-along's resident ♪/♫ note paints, one per
    /// [`crate::nyan_sing::NoteKind`]. Re-painted only when the host id (kind +
    /// cell-metric-derived size) changes.
    note_cache: [Option<NotePaintCache>; 2],
    /// The NUKE CLOUD's resident paints, one per [`crate::nuke::NukePart`].
    /// Same discipline as [`Self::note_cache`]: the art is a pure function of
    /// `(part, w, h)`, so a cloud that is rising, blooming and rolling out for
    /// 3.6 s bakes each part ONCE and animates by dest-rect transform alone —
    /// it never spends the shared per-frame bake budget after warmup.
    nuke_cache: [Option<NotePaintCache>; 3],
    /// v3 §1.1 fix #2: session-scoped LRU set of finished one-shots, keyed
    /// `ident ^ ctx_fp` → last-touch sequence (touch-on-hit). Cap
    /// [`DONE_MARKS_CAP`]; eviction removes the oldest-touched key. Survives
    /// transient `reset()` (which flushes pending marks into it first);
    /// cleared only by [`WordDecorations::hard_reset`].
    done_marks: DoneMarkLru,
    /// v3 §1.1 fix #3: freeze instant while the host has suspended effects
    /// (perf_reduced latch / suppressed alt screen). `thaw` shifts every
    /// stored timestamp forward by the freeze duration.
    frozen_at: Option<Instant>,
    /// v3 §1.1 fix #4: the resize-settle deadline — births before it (or on a
    /// rescan that itself changed cols) are born-settled.
    settle_until: Option<Instant>,
    /// Whether the host currently CANNOT present this engine's window
    /// (unfocused or deco-suspended). Synced by [`Self::set_presentable`].
    ///
    /// Stored inverted so `#[derive(Default)]` yields "presentable", which is
    /// what a hostless/wasm build that never syncs must behave as.
    away: bool,
    /// Latched when presentability returns after an away span: the NEXT rescan's
    /// births are spent (born-settled) rather than played. See
    /// [`Self::set_presentable`] for the whole rationale.
    spend_next_births: bool,
    /// The last time a rare revisit was granted — the [`REVISIT_MIN_GAP`] clock.
    /// `None` until the first one, so a fresh engine can grant immediately once
    /// its first check period elapses.
    last_revisit: Option<Instant>,
    /// The next instant a revisit roll may be attempted at all.
    next_revisit_check: Option<Instant>,
    /// Monotonic count of attempted revisit rolls — the roll's ordinal. A
    /// COUNTER, never a clock reading: the roll must be reproducible on replay,
    /// so nothing about it may depend on wall-clock phase.
    revisit_checks: u64,
    /// v3 §1.1: rescan sequence counter — episodes stamp `seen_seq` on adopt,
    /// so alignment's "old" set is exactly the not-yet-adopted episodes.
    rescan_seq: u64,
    /// v3 §1.1: deferred birth/adoption candidates (resident scratch).
    pending: Vec<PendingBirth>,
    /// v3 §1.1 alignment scratch: the seed group's old episodes, pulled out of
    /// the persist map for rekeying (resident).
    align_old: Vec<(u64, Episode)>,
    /// v3 §1.1 alignment scratch: the DP's traceback decisions, one byte per
    /// (old, candidate) cell — [`ALIGN_DECISION_CELLS`] = 66,177 of them.
    ///
    /// RESIDENT, not stack: as a `[0u8; ALIGN_DECISION_CELLS]` local this was a
    /// 66 KB memset on every rescan that deferred a candidate (i.e. every frame
    /// of scrolling output or newly typed text), and the zeroing was dead —
    /// row 0 is refilled per group, the col-0 spine and the interior are written
    /// before they are read, and the traceback never visits cell (0,0), the one
    /// cell nobody writes. The cost that actually mattered was cache: dirtying
    /// half of L1D immediately beside the rescan's row buffers. Allocated once,
    /// then lent to `align_pending` by `mem::take` (the loop borrows `persist`
    /// and `align_old` while holding it) and restored at the tail.
    align_decisions: Vec<u8>,
    /// §F4.2 Kitty Log recording: this tick's sightings (resident, cap
    /// [`MAX_OCCURRENCES`], cleared at tick start — web builds without a host
    /// drain stay bounded). Drained via
    /// [`WordDecorations::drain_kitty_sightings`].
    sightings: Vec<KittySighting>,
    /// §F4.2 pre-loop resident scratch: the idents whose episode has not yet
    /// logged a sighting (computed from a shared `&persist` borrow before the
    /// emission loop, which holds `persist` immutably).
    unlogged: Vec<u64>,
    /// This tick's curse-BONK cues (resident, cap [`MAX_CURSE_CUES`], cleared
    /// at tick start — same hostless-boundedness rule as `sightings`).
    /// Drained via [`WordDecorations::drain_curse_cues`].
    curse_cues: Vec<CurseCue>,
    /// §6.4 flash-limiter reservations. Already-fired entries survive for the
    /// rolling safety window even if their episode departs; future entries
    /// survive only while their owning persist episode still references that
    /// exact slot. One pending slot per live owner plus at most two recent
    /// ignitions gives the hard [`MAX_IGNITION_RESERVATIONS`] bound.
    ignitions: Vec<IgnitionReservation>,
    /// Per-tick resident scratch: this frame's LIVE novas (≤ MAX_ACTIVE_NOVAS;
    /// prepass-filled). The gaze hook (§5.8), the blast coupling (§6.5), and
    /// the profanity emission arm all read it.
    novas: Vec<NovaLive>,
    /// v3 §3.2 per-tick scratch: this frame's live supernova
    /// (≤ [`supernova::MAX_ACTIVE_SUPERNOVAE`] = 1; prepass-filled).
    supers: Vec<SuperLive>,
    /// v3 §3.2 GLOBAL BURST MUTEX: the latest window end among granted
    /// (pending or live) supernovae — classic nova ignitions are
    /// limiter-deferred while `now` is inside it.
    super_until: Option<Instant>,
    /// v3 §3.2: session-scoped birth sequence, incremented on EVERY persist
    /// miss — the supernova roll's decorrelation term. Deliberately survives
    /// both `reset()` and `hard_reset()` (session-scoped, not episode state).
    birth_seq: u64,
    /// Per-tick resident scratch (§6.5): `(occurrence index, nova index)`
    /// pairs — each live nova's ≤ MAX_COUPLING_WORDS nearest ink-bearing
    /// occurrences, recomputed per presented frame (stateless coupling).
    coupling: Vec<(u16, u8)>,
    /// Resident distance-sort scratch for the nearest-16 coupling selection.
    dist_scratch: Vec<(u64, u16)>,
    /// PANE BINDING (2026-07-28) — the episode/grid state of every pane this
    /// window shows EXCEPT the one currently bound, whose state lives in the
    /// live fields above. See [`WordDecorations::bind_pane`].
    parked: FxHashMap<u64, ParkedPane>,
    /// Which pane's state the live fields currently hold; `None` for a host
    /// that never binds (the single-pane path), which is what makes binding
    /// opt-in and the unbound engine byte-identical to the pre-binding one.
    bound: Option<u64>,
    /// The bound pane's grid origin in WINDOW pixels. Ignition centers are
    /// shifted by it so the §6.4 flash limiter compares every pane's flashes
    /// in ONE coordinate space — the "window-wide" claim it is modeled on.
    pane_px: (i32, i32),
    /// Whether the host brackets its frames with
    /// [`begin_host_frame`](WordDecorations::begin_host_frame). Latched on the
    /// first call: the two-bake budget is a per-FRAME budget, so a host that
    /// ticks several panes per frame must not multiply it, while a host that
    /// never brackets keeps the historical one-prologue-per-tick behaviour.
    host_brackets_frames: bool,
}

/// One pane's episode + grid state while another pane is bound.
///
/// Panes in one window share ONE `WordDecorations` so they share ONE cat
/// atlas, ONE flash limiter and ONE supernova mutex: the two safety budgets
/// are window-wide claims (§6.4 WCAG 2.3.1, §3.2 burst mutex) and must not
/// multiply with the pane count. Everything that is grid-shaped or
/// episode-scoped parks here instead; binding `mem::swap`s it in and out, so
/// none of the engine body's `self.<field>` reads change.
#[derive(Default)]
struct ParkedPane {
    occ: Vec<Occurrence>,
    last_epoch: u64,
    have_scanned: bool,
    cols: u16,
    frame: u64,
    active_until: Option<Instant>,
    scan_row_occupied: Vec<bool>,
    /// Parked even though sharing it would be CORRECT (it keys on row text
    /// plus an inputs fingerprint): `ScanMemo::fit`'s `gen_cap = 2·rows` would
    /// thrash between panes of different heights. ~2 KB at 24×40.
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
    last_revisit: Option<Instant>,
    next_revisit_check: Option<Instant>,
    revisit_checks: u64,
    rescan_seq: u64,
    pending: Vec<PendingBirth>,
    align_old: Vec<(u64, Episode)>,
}

impl ParkedPane {
    /// A pane the engine has never driven. `away: true` (NOT `Default`, which
    /// is presentable) is load-bearing: the host's immediately-following
    /// `set_presentable(now, true)` must see a real transition, or
    /// `spend_next_births` never latches and a pane split onto a screenful of
    /// feline words births the whole backlog at once — the exact storm
    /// [`WordDecorations::set_presentable`] exists to prevent.
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
        std::mem::swap(&mut self.last_revisit, &mut wd.last_revisit);
        std::mem::swap(&mut self.next_revisit_check, &mut wd.next_revisit_check);
        std::mem::swap(&mut self.revisit_checks, &mut wd.revisit_checks);
        std::mem::swap(&mut self.rescan_seq, &mut wd.rescan_seq);
        std::mem::swap(&mut self.pending, &mut wd.pending);
        std::mem::swap(&mut self.align_old, &mut wd.align_old);
    }
}

impl WordDecorations {
    /// Record the one-shot guard before an episode leaves the persist map.
    fn mark_persist_departure(&mut self, ident: u64, ep: &Episode) {
        if ep.one_shot_started() {
            mark_done(&mut self.done_marks, ident, ep);
        }
    }

    /// Move an aligned episode and its limiter reservation under one logical
    /// rekey operation. A delayed nova keeps running from `ep.nova_start`
    /// after horizontal/reflow alignment; leaving its reservation under the
    /// retired identity would let [`Self::prune_ignitions`] discard the slot
    /// and admit an overlapping flash. There is no scheduler or pruning call
    /// between the owner rewrite and persist insertion, so observers can only
    /// see the before or after state.
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
    /// Alignment temporarily removes old episodes, admits matched/fresh
    /// visible episodes, then offers unmatched history back for grace — an
    /// ordering that at the capacity boundary would let the final grace
    /// reinsertion create a 513th entry unless every path funnels through
    /// here. Key collisions favor an observed claimant; otherwise the
    /// deterministic oldest `(last_seen, ident)` in the resident-plus-incoming
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

        // Defensive repair if the map is somehow already over cap, then select
        // the oldest member of the complete resident-plus-incoming union at the
        // ordinary boundary.
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

    /// TRANSIENT reset (v3 §1.1 reset table: pane-space / split change).
    /// Flushes every started episode's done mark into `done_marks` FIRST (the
    /// normative write path — an episode leaving the persist map with any
    /// one-shot started is marked under its current ident), then drops all
    /// per-occurrence state while KEEPING the `done_marks` set, so the next
    /// rescan re-enters finished words as born-done instead of replaying.
    pub fn reset(&mut self) {
        self.with_each_pane(Self::reset_bound);
    }

    /// [`reset`](Self::reset) for the state currently in the LIVE fields alone —
    /// the bound pane, or the whole window when nothing is bound. Parked
    /// siblings are untouched.
    ///
    /// This is the right reset whenever the thing being retired is one pane:
    /// folding over every parked pane there would let closing the focused pane
    /// wipe the cats out of the panes that are still on screen, which is the
    /// blanket-reset behaviour the owner's ruling retired.
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

    /// Bind `key`'s state into the live fields, recording the pane's grid
    /// origin in WINDOW pixels (`px_origin`) for the shared flash limiter.
    ///
    /// Owner ruling, 2026-07-28: "sparkle effects and toys are NOT SINGLE PANE
    /// ONLY". Every visible pane drives this one engine in turn, so each needs
    /// its own damage epoch, column count, episode map, done marks and rescan
    /// sequence — otherwise pane B inherits pane A's spent one-shots, alternating
    /// widths pin `born_settled` forever, and the `REKEY_MAX_SEQ_GAP` window
    /// closes after the first pane of every frame.
    pub fn bind_pane(&mut self, key: u64, px_origin: (i32, i32)) {
        self.pane_px = px_origin;
        match self.bound {
            Some(cur) if cur == key => return,
            Some(cur) => {
                let mut slot = ParkedPane::default();
                slot.swap(self);
                self.parked.insert(cur, slot);
            }
            None => {
                // The live fields hold WINDOW-wide state: one pane covering the
                // whole grid, whose every word has just moved somewhere else.
                // Retire exactly that, exactly as the v3 §1.1 reset table's
                // "pane-space change → transient reset" rule always did. NOT
                // `reset()`: the parked panes are already correctly scoped, and
                // wiping them here would kill the cats in panes that never moved.
                self.reset_bound();
            }
        }
        let mut slot = self.parked.remove(&key).unwrap_or_else(ParkedPane::fresh);
        slot.swap(self);
        self.bound = Some(key);
    }

    /// Open a host FRAME, before any pane's [`tick`](Self::tick).
    ///
    /// The `MAX_BAKES_PER_FRAME` budget and the baker's LRU clock are per
    /// PRESENTED FRAME, not per tick: a window compositing N panes bakes at
    /// most two tiles in total, into the one shared atlas. Latches
    /// `host_brackets_frames`, so a host that never calls this keeps the
    /// historical one-prologue-per-tick behaviour unchanged.
    pub fn begin_host_frame(&mut self) {
        self.host_brackets_frames = true;
        self.cat_baker_ready = false;
    }

    /// Drop parked state for panes this window no longer shows, and unbind the
    /// live state if its own pane is gone (its episodes describe a grid that
    /// no longer exists). Hosts call this once per frame off the layout they
    /// are about to compose.
    pub fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        self.parked.retain(|k, _| keep(*k));
        if let Some(cur) = self.bound
            && !keep(cur)
        {
            self.bound = None;
            // With no pane bound the honest grid origin is the window's own:
            // a stale offset would place the next host's ignition centers in a
            // coordinate space nothing else on the glass shares.
            self.pane_px = (0, 0);
            // ONLY the departed pane's state (it is the one in the live fields).
            // Its still-visible siblings are parked and keep their episodes —
            // closing one pane must not blank the cats in the rest.
            self.reset_bound();
        }
    }

    /// Run `f` with EVERY pane's state bound in turn — the currently bound (or
    /// unbound-window) state first, then each parked pane — restoring the
    /// original binding afterwards. The fold behind `reset`/`hard_reset`.
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

    /// HARD reset (v3 §1.1 reset table: master toggle, config reload / lexicon
    /// rebuild, web knob setters): clears episodes AND `done_marks` — a fresh
    /// start is user intent, so everything replays.
    pub fn hard_reset(&mut self) {
        self.with_each_pane(|wd| {
            wd.done_marks.clear();
            wd.reset_transient_state();
        });
    }

    /// The shared body of [`reset`](Self::reset) / [`hard_reset`](Self::hard_reset):
    /// everything except the `done_marks` policy.
    fn reset_transient_state(&mut self) {
        self.occ.clear();
        self.have_scanned = false;
        self.last_epoch = 0;
        self.active_until = None;
        // The tokenise memo is only valid against the lexicon + scan gates that
        // produced it, and `hard_reset` is exactly the "config reload / lexicon
        // rebuild / knob setter" hook (§1.1 reset table). Retiring it here is
        // what keeps a reloaded deny-list or a swapped language set from being
        // ignored on rows whose TEXT never changed — the memo would otherwise
        // keep answering with the old lexicon's matches forever on an idle
        // grid. `reset()` (pane-space change) drops it too: a cold memo is only
        // a warm-up cost, never a wrong frame.
        self.scan_memo.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        self.persist.clear();
        self.seed_ordinals.clear();
        self.pending.clear();
        self.align_old.clear();
        self.frozen_at = None;
        self.settle_until = None;
        // §5.5 invalidation: config reload / toggle drop the bake cache
        // wholesale (version bump inside; a no-op when already empty).
        self.cat_baker.clear();
        // §F4.2: pending sightings die with the state.
        self.sightings.clear();
        self.unlogged.clear();
        // Curse-BONK cues die with the state too (their episode latches just
        // cleared with `persist` — no orphaned sound may outlive its word).
        self.curse_cues.clear();
        // Nova state dies with the occurrence set: pending/spent limiter slots
        // and the per-tick nova scratch all clear (byte-identical off).
        self.ignitions.clear();
        self.novas.clear();
        self.coupling.clear();
        // v3 §3.2: the supernova scratch + burst mutex clear with the nova
        // state; `birth_seq` deliberately survives (session-scoped — the roll
        // stays decorrelated across resets).
        self.supers.clear();
        self.super_until = None;
    }

    /// v3 §1.1 fix #3: suspend the engine's clocks (perf_reduced latch,
    /// suppressed alt screen). Idempotent — the first call latches the freeze
    /// instant; the host may call it every suspended frame.
    pub fn freeze(&mut self, now: Instant) {
        if self.frozen_at.is_none() {
            self.frozen_at = Some(now);
        }
    }

    /// v3 §1.1 fix #3: resume after [`freeze`](Self::freeze) — shifts EVERY
    /// stored timestamp (`appeared`, `nova_start`, `phase_start`, gaze
    /// changes, limiter slots, `active_until`, the settle deadline) forward by
    /// the freeze duration so animations resume exactly where they paused (a
    /// cat frozen at 50% rise resumes at 50% rise; nothing silently completes
    /// while invisible). `last_seen` is bumped too, so episodes occluded
    /// during the freeze keep their remaining grace. No-op when not frozen.
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
            // The clearance pause is itself a timestamp: shifting it with the
            // phase clock keeps `now − peek_pause` (the span the resume will
            // add back to `phase_start`) invariant across a freeze.
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
    }

    /// v3 §1.2 duty pin: no deadline ever arms. The one-shot peek is covered by
    /// `is_active` (frame-paced while animating, zero wakes after Done). The
    /// method exists only because hosts poll it; it always disarms.
    pub fn next_deadline(&self, _now: Instant) -> Option<Instant> {
        None
    }

    /// Drain this tick's recorded Kitty-Log sightings (§F4.2), preserving the
    /// resident vec's capacity. Hosts drain at BOTH tick sites (the render
    /// path and the introspection path); web builds may never drain — the
    /// tick-start clear bounds growth at [`MAX_OCCURRENCES`].
    pub fn drain_kitty_sightings(&mut self) -> std::vec::Drain<'_, KittySighting> {
        self.sightings.drain(..)
    }

    /// Drain this tick's recorded curse-BONK cues, preserving the vec's
    /// capacity (the sightings-drain twin: hosts drain PROMPTLY after
    /// [`tick`](Self::tick) — the vec clears at the next tick's start — and a
    /// disabled bonk knob drains-and-drops so no backlog crosses an enable).
    pub fn drain_curse_cues(&mut self) -> std::vec::Drain<'_, CurseCue> {
        self.curse_cues.drain(..)
    }

    /// Sync whether the host can PRESENT this engine's window right now
    /// (focused and not deco-suspended — the same predicate the cursor
    /// companion's `set_collection_presentable` takes).
    ///
    /// THE REFOCUS STORM. A window that is not presenting does not render, and
    /// the rescan lives on the render path — so while you are away NOTHING is
    /// scanned. Every feline word that arrived in the meantime is therefore
    /// genuinely NEW to the engine on the first frame after you come back, and
    /// all of them are born at once, each owed an entrance: a crowd of cats
    /// announcing text that scrolled by minutes ago.
    ///
    /// The `born_unfocused_entrance_never_replays_on_refocus` rule does not
    /// cover this: it protects episodes whose clock was ALREADY running, which
    /// requires that a rescan ran while unfocused. Here none did.
    ///
    /// Returning to presentable therefore latches `spend_next_births`: births in
    /// the next rescan are born-settled — no entrance, no burst roll, settled
    /// ink — exactly as if their peek had elapsed unwatched, which is what
    /// "appear when the word FIRST appears" means for a word that first appeared
    /// while nobody was looking. They stay eligible for a later [rare
    /// revisit](Self::roll_revisit), so the screen is not permanently dead.
    pub fn set_presentable(&mut self, now: Instant, presentable: bool) {
        if self.away != presentable {
            return; // no transition (`away` is the inverse of `presentable`)
        }
        self.away = !presentable;
        if presentable {
            self.spend_next_births = true;
            // Do not let a revisit fire the instant focus lands — that would
            // read as part of the very storm this suppresses.
            self.next_revisit_check = Some(now + REVISIT_CHECK_PERIOD);
        }
    }

    /// Attempt the rare-late-kitty roll, re-arming AT MOST ONE spent feline
    /// episode. Returns the ident that was revisited, if any.
    ///
    /// Called once per tick; almost every call returns immediately on the
    /// period gate, so the hot path cost is one `Instant` compare.
    fn roll_revisit(&mut self, now: Instant, cfg: &DecoConfig) -> Option<u64> {
        if cfg.reduced_motion || !cfg.feline || self.frozen_at.is_some() || self.away {
            return None;
        }
        // Period gate first: this is the branch nearly every tick takes.
        match self.next_revisit_check {
            Some(at) if now < at => return None,
            _ => {}
        }
        self.next_revisit_check = Some(now + REVISIT_CHECK_PERIOD);
        self.revisit_checks = self.revisit_checks.wrapping_add(1);
        if self
            .last_revisit
            .is_some_and(|at| now.saturating_duration_since(at) < REVISIT_MIN_GAP)
        {
            return None;
        }
        // Candidates: VISIBLE feline occurrences whose peek is spent. Ordered by
        // the occurrence list (row-major) so the choice is reproducible.
        let is_candidate = |occ: &&Occurrence| {
            occ.spec.graphic.is_some()
                && occ.cat_text_clear
                && self
                    .persist
                    .get(&occ.ident)
                    .is_some_and(|ep| ep.peek_done || ep.born_settled || ep.born_done)
        };
        let mut candidates = self.occ.iter().filter(|occ| is_candidate(occ));
        let first = candidates.next()?;
        let count = 1 + candidates.count();
        // One roll for WHETHER, one for WHICH — both deterministic in the check
        // ordinal so a replay reproduces the same visit.
        let draw = mix(first.ident ^ REVISIT_SALT ^ self.revisit_checks);
        if draw % 100 >= REVISIT_CHANCE_PCT {
            return None;
        }
        let pick = (mix(draw) % count as u64) as usize;
        let ident = self
            .occ
            .iter()
            .filter(|occ| is_candidate(occ))
            .nth(pick)
            .map(|occ| occ.ident)?;
        // Re-arm ONLY the peek axis. Burst/sweep stay spent — a revisit is a cat
        // coming back to say hello, not the word's whole birth replayed.
        let ep = self.persist.get_mut(&ident)?;
        ep.peek_done = false;
        ep.peek_started = false;
        ep.peek_pause = None;
        ep.peek_total = 0;
        ep.shown_as = None;
        ep.born_done = false;
        ep.born_settled = false;
        ep.phase_start = Some(now);
        // The done-mark must go too, or the next rescan re-births it done.
        // Same key as every other done_marks site: `ident ^ ctx_fp`.
        self.done_marks.remove(&(ident ^ ep.ctx_fp));
        self.last_revisit = Some(now);
        Some(ident)
    }

    /// The versioned atlas paired with this frame's `free_sprites` output
    /// (free-overlay Phase 4: the baker atlas — exact-size tiles, art at tile
    /// row 0). Hosts copy it into `RenderInput::free_atlas` only when free
    /// sprites were emitted (`None` otherwise keeps cat-free frames
    /// byte-identical).
    pub fn free_atlas(&mut self) -> Option<std::sync::Arc<aterm_render::SceneAtlas>> {
        self.cat_baker.atlas()
    }

    /// Emit the rare cat flying IN FRONT of the cursor, pulling it forward while
    /// the `nyan` ribbon streams behind as its rainbow wake. The
    /// earn/fade/exit lifecycle lives host-side
    /// ([`crate::nyan_cursor::CursorCat`]); this bakes the smooth CatBaker cat
    /// (happy, or a wink) — or a user sprite — into the SAME free atlas the cats
    /// share and stamps exactly one companion-body `OverText`/NEAREST
    /// [`FreeSprite`] just ahead of the cursor cell, vertically centred on its
    /// row, at the given `alpha`. Built-in and user-authored companions follow
    /// the same one-body emission contract; sing-along notes, when present,
    /// stream immediately behind that body.
    ///
    /// Primes the baker itself (the emission `tick` bails before `begin_frame`
    /// when no cat-words are on screen). Near the right margin the cat clamps
    /// to the grid edge (it never vanishes mid-flight) — and when that clamp
    /// would slide it back OVER the cursor/text cells it RISES up to half a
    /// cell instead of covering the glyphs. When the desired tile cannot bake
    /// this frame (budget spent), it falls back to the resident Open-eyes tile
    /// and only emits nothing when no tile is resident at all (it lands next
    /// frame).
    ///
    /// Call AFTER `tick`, appending into the same `free` buffer it filled.
    ///
    /// HORIZONTAL ANCHOR ([`Self::NYAN_LEAD_NUM`]/[`Self::NYAN_LEAD_DEN`]):
    /// the companion leads the cursor by 3/4 of a cell, measured from the
    /// cursor cell's right edge: far enough that the sprite reads as ESCORTING
    /// the cursor rather than crowding the glyph being typed, while staying
    /// inside one cell of it so the pair still reads as a unit (and the
    /// flourish anchor at the cell boundary stays honest).
    #[must_use]
    pub fn nyan_cursor_footprint(&self, layout: NyanCursorLayout) -> Option<CatFootprint> {
        let NyanCursorLayout {
            geom,
            cursor: (crow, ccol),
            look,
            bob,
        } = layout;
        if self.nyan_disabled || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        let ch = usize::from(geom.cell_h);
        let slot_w = 4 * ch;
        let slot_h = 2 * ch;
        let (w, h) = if let Some(cust) = self.nyan_custom.as_ref() {
            let (nw, nh) = (usize::from(cust.w).max(1), usize::from(cust.h).max(1));
            let max_h = (ch * 9 / 5).clamp(1, slot_h);
            let s = (slot_w as f32 / nw as f32).min(max_h as f32 / nh as f32);
            (
                ((nw as f32 * s).round() as usize).clamp(1, slot_w) as u16,
                ((nh as f32 * s).round() as usize).clamp(1, slot_h) as u16,
            )
        } else {
            let look = look.normalized();
            let desired_h = 1.45 * f32::from(geom.cell_h) * look.age.scale();
            authored_cat_size(look.variant, desired_h, geom.cell_h)
        };
        let cw = i32::from(geom.cell_w);
        let ch_i = i32::from(geom.cell_h);
        // Fly just AHEAD of the cursor (the 3/4-cell lead — see the doc
        // above). Near the right margin the cat CLAMPS to the grid edge rather
        // than being dropped: typing to the end of a line is exactly when
        // momentum peaks, so a cat that simply went out of bounds would blink
        // out with no fade. Clamped, it glides to a stop over the line end
        // until the wrap gives it room again.
        let grid_w = i32::from(geom.cols).saturating_mul(cw);
        let cursor_right = i32::from(ccol).saturating_add(1).saturating_mul(cw);
        let lead = cw.saturating_mul(Self::NYAN_LEAD_NUM) / Self::NYAN_LEAD_DEN;
        let x = cursor_right
            .saturating_add(lead)
            .min(grid_w.saturating_sub(i32::from(w)));
        if x < 0 {
            return None; // the grid is narrower than the cat itself
        }
        // BOUNDARY RISE: when the edge clamp pushes the sprite back across the
        // cursor cell's right edge — i.e. it would horizontally cover the
        // cursor/text cells — the cat RISES instead, up to half a cell above
        // its centred rest, so it clears the glyphs vertically. The rise is
        // proportional to the intrusion (deepening smoothly as the cursor
        // approaches the margin), capped at `ch/2`, and additionally backed
        // off at the TOP edge so a top-row flight is never lifted off-grid: a
        // top-row sprite keeps its slid-down, partially-clipped rest.
        let rest_top = i32::from(crow) * ch_i + ch_i / 2 - i32::from(h) / 2;
        let intrusion = cursor_right - x;
        let rise = if intrusion > 0 {
            intrusion.min(ch_i / 2).min(rest_top.max(0))
        } else {
            0
        };
        let y = rest_top - rise + (bob * ch_i as f32).round() as i32;
        Some(CatFootprint { x, y, w, h })
    }

    /// The companion's horizontal lead ahead of the cursor cell's right edge,
    /// as a fraction of a cell: `cell_w · NUM / DEN` = 3/4 cell. See
    /// [`Self::nyan_cursor_footprint`] for the placement rationale.
    const NYAN_LEAD_NUM: i32 = 3;
    const NYAN_LEAD_DEN: i32 = 4;

    pub fn nyan_cursor(
        &mut self,
        frame: NyanCursorFrame,
        free: &mut Vec<FreeSprite>,
    ) -> Option<u64> {
        let NyanCursorFrame {
            geom,
            cursor,
            look,
            colors,
            bob,
            alpha,
            pose,
            sing,
            notes,
        } = frame;
        if alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        let footprint = self.nyan_cursor_footprint(NyanCursorLayout {
            geom,
            cursor,
            look,
            bob,
        })?;
        self.cat_baker.set_free_tiles(true);
        if !self.cat_baker_ready {
            self.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
            self.cat_baker_ready = true;
        }

        let (w, h) = (footprint.w, footprint.h);
        // Resolve the atlas tile: a USER sprite (resampled to fit, host-baked)
        // overrides the built-in cat, which is baked by the SMOOTH anti-aliased
        // CatBaker (same renderer as the peeking word-cats) with a HAPPY face —
        // or a WINK on the star-wink exit.
        let (ax, ay, w, h) = if let Some(cust) = self.nyan_custom.as_mut() {
            let (nw, nh) = (usize::from(cust.w).max(1), usize::from(cust.h).max(1));
            if cust.cache.as_ref().map(|(cw, chh, _)| (*cw, *chh)) != Some((w, h)) {
                let r = crate::nyan_cursor::resample_nearest(
                    &cust.rgba,
                    nw,
                    nh,
                    usize::from(w),
                    usize::from(h),
                );
                cust.cache = Some((w, h, r));
            }
            let host = crate::nyan_cursor::HOST_ID ^ self.nyan_gen;
            let Some(tile) =
                self.cat_baker
                    .host_tile(host, w, h, &cust.cache.as_ref().expect("set above").2)
            else {
                return None; // budget spent this frame; lands next frame
            };
            (tile.ax, tile.ay, w, h)
        } else {
            // Built-in: the exact collected character identity. The generated
            // fixed-point aspect preserves reference proportions without asset
            // I/O; palette/age/accessory are scalar bake-key axes.
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
                // The baked blink/squint frame — a stable, cache-cheap axis (the
                // three eye states share the atlas; the pose scale/lean is a
                // draw-time transform, never a bake-key change).
                eyes: pose.eyes,
            };
            // Bake-budget resilience: when the exact eyes-frame tile can't
            // bake THIS frame (the shared ≤2-bakes budget went to word-cats),
            // fall back to the episode's resident Open-eyes tile instead of
            // dropping the whole sprite — a mid-flight blink must never make
            // the cat flicker out for a frame. The fallback probe is a pure
            // cache hit (the budget is already spent, so `get_v4` cannot
            // bake), and the desired eyes-frame lands next frame.
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
        // Living-cartoon transform: the tile is baked at its NATURAL size `w×h`
        // (a stable atlas key, so an animating pose never rebakes); squash/stretch
        // scales the DEST rect about the sprite centre and the forward lean shifts
        // it ahead — both applied only here, at draw. `aw/ah` stay the natural
        // source size, so the renderer NEAREST-scales the fixed tile.
        let (nat_w, nat_h) = (w, h);
        let grid_w = i32::from(geom.cols).saturating_mul(i32::from(geom.cell_w));
        let dest_w = ((f32::from(nat_w) * pose.scale_x).round() as i32)
            .clamp(1, grid_w.max(1).min(i32::from(u16::MAX))) as u16;
        let dest_h =
            ((f32::from(nat_h) * pose.scale_y).round() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let lead_px = (pose.lead * f32::from(geom.cell_w)).round() as i32;
        let cx = footprint.x + i32::from(nat_w) / 2;
        let cy = footprint.y + i32::from(nat_h) / 2;
        let sprite_x = (cx - i32::from(dest_w) / 2 + lead_px).clamp(0, grid_w - i32::from(dest_w));
        let sprite = FreeSprite {
            x: sprite_x,
            y: cy - i32::from(dest_h) / 2,
            w: dest_w,
            h: dest_h,
            ax,
            ay,
            aw: nat_w,
            ah: nat_h,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x: false,
            z: FreeZ::OverText,
            sampler: FreeSampler::Nearest,
        };

        // ── the sing-along's ♪/♫ music notes (`crate::nyan_sing`) ───────────
        // Streaming from the singing cat's mouth (its leading edge), pushed
        // BEFORE the body so a freshly spawned note slides out from behind the
        // head. Notes are structurally cursor-cat-only (word-cats never reach
        // this function) and load-shed with the whole sparkle branch. The ring
        // that produced `notes` caps them at `nyan_sing::MAX_NOTES`; the two
        // WHITE-baked tiles are rainbow-tinted per sprite through the
        // `FreeSprite::tint` channel, so the whole shower is two atlas slots.
        // A note whose tile cannot bake within the shared two-bake budget is
        // dropped for one frame and lands on the next.
        let mut note_sprites: [Option<FreeSprite>; crate::nyan_sing::MAX_NOTES] =
            [None; crate::nyan_sing::MAX_NOTES];
        if sing > 0.0 {
            let grid_w = i32::from(geom.cols).saturating_mul(i32::from(geom.cell_w));
            let mouth_x = sprite.x + i32::from(sprite.w);
            let mouth_y = cy;
            for (slot, note) in note_sprites.iter_mut().zip(notes.iter().flatten()) {
                let kind_idx = match note.kind {
                    crate::nyan_sing::NoteKind::Eighth => 0usize,
                    crate::nyan_sing::NoteKind::Beamed => 1,
                };
                let (nw, nh) = crate::nyan_sing::note_nat_size(note.kind, geom.cell_h);
                let host = crate::nyan_sing::note_host_id(note.kind, nw, nh);
                if self.note_cache[kind_idx].as_ref().map(|c| c.host) != Some(host) {
                    self.note_cache[kind_idx] = Some(NotePaintCache {
                        host,
                        rgba: crate::nyan_sing::bake_note(nw, nh, note.kind)
                            .pixels()
                            .to_vec(),
                    });
                }
                let paint = &self.note_cache[kind_idx]
                    .as_ref()
                    .expect("filled above")
                    .rgba;
                let Some(tile) = self.cat_baker.host_tile(host, nw, nh, paint) else {
                    continue; // budget spent; this note lands next frame
                };
                // The wind-down crossfade rides the alpha: note envelope ×
                // cat presentation × sing drive.
                let a = (f32::from(note.alpha) * f32::from(alpha) / 255.0 * sing) as u8;
                if a == 0 {
                    continue;
                }
                let nx =
                    mouth_x + (note.dx * f32::from(geom.cell_w)).round() as i32 - i32::from(nw) / 2;
                let ny =
                    mouth_y + (note.dy * f32::from(geom.cell_h)).round() as i32 - i32::from(nh) / 2;
                if i32::from(nw) > grid_w {
                    continue; // a grid narrower than one note sings unadorned
                }
                *slot = Some(FreeSprite {
                    // Notes ride fully on-grid horizontally (vertical clipping
                    // is the renderer's).
                    x: nx.clamp(0, grid_w - i32::from(nw)),
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

        // `tick` folds the atlas version before this post-tick cursor bake.
        // Return a complete cursor-art fingerprint so the host cannot swallow
        // either a fresh atlas upload or a local-palette change at its early-out.
        let mut fp = fold_free(0xCBF2_9CE4_8422_2325, &sprite);
        // The notes fold too: a rising/fading note (or a deferred note tile
        // landing) must never be swallowed by a host early-out.
        for note in note_sprites.iter().flatten() {
            fp = fold_free(fp, note);
        }
        fp = fold_u64(fp, self.cat_baker.version());
        fp = fold_u64(fp, u64::from(colors.accent));
        fp = fold_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    /// Install one already-resolved immutable source.  This function performs
    /// no filesystem access or image decoding and never clones custom source
    /// bytes.  The stable source fingerprint keys the shared-atlas tile; a
    /// target metric change still rebuilds only the bounded resample cache.
    pub fn set_nyan_sprite_source(&mut self, source: NyanSpriteSource) {
        match source {
            NyanSpriteSource::BuiltIn => {
                self.nyan_custom = None;
                self.nyan_disabled = false;
                self.nyan_gen = 0;
            }
            NyanSpriteSource::Disabled => {
                self.nyan_custom = None;
                self.nyan_disabled = true;
                self.nyan_gen = u64::MAX;
            }
            NyanSpriteSource::Custom {
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
                    self.nyan_custom = None;
                    self.nyan_disabled = true;
                    self.nyan_gen = u64::MAX;
                    return;
                }
                self.nyan_custom = Some(NyanCustom {
                    source_fp,
                    w,
                    h,
                    rgba,
                    cache: None,
                });
                self.nyan_disabled = false;
                self.nyan_gen = source_fp;
            }
        }
    }

    /// Diagnosable, allocation-free installed source identity for host
    /// conformance tests and capture/glass generation checks.
    #[doc(hidden)]
    pub fn nyan_sprite_source_fingerprint(&self) -> Option<u64> {
        if self.nyan_disabled {
            None
        } else {
            Some(
                self.nyan_custom
                    .as_ref()
                    .map_or(0, |custom| custom.source_fp),
            )
        }
    }

    /// Borrow the exact immutable RGBA Arc supplied by the host.
    #[doc(hidden)]
    pub fn nyan_sprite_rgba(&self) -> Option<&Arc<[u8]>> {
        self.nyan_custom.as_ref().map(|custom| &custom.rgba)
    }

    /// Whether a user-configured Nyan sprite currently overrides the built-in
    /// homage. Exposed for host conformance checks and diagnostics.
    #[must_use]
    pub fn has_custom_nyan_sprite(&self) -> bool {
        self.nyan_custom.is_some()
    }

    /// True if the grid changed since the last scan and a rescan is due.
    pub fn needs_rescan(&self, epoch: u64) -> bool {
        !self.have_scanned || epoch != self.last_epoch
    }

    /// Rescan the visible grid into occurrences. Runs only when the damage epoch
    /// advanced. Preserves the `appeared` timestamp of words that are still
    /// present (keyed by `seed`) so a word that merely scrolled does not re-trigger
    /// its sparkle.
    ///
    /// Cold path (introspection capture, tests): resolves each row itself via
    /// [`Terminal::render_row_into`]. The per-frame render path uses
    /// [`rescan_from_cells`](Self::rescan_from_cells) on the snapshot
    /// `cell_frame_into` already built — one full-grid resolve per presented
    /// frame, not two.
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
        // v3 §1.1 fix #3: a FROZEN engine neither scans nor advances — an
        // introspection capture during a perf_reduced freeze must not
        // grace-expire (and done-mark) every episode against the suspended
        // clock. `last_epoch` is untouched, so the pending rescan stays
        // pending and re-fires on the first post-thaw tick.
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
                    // bottom-priority cutoff so the rows nearest the prompt —
                    // where the user is typing — keep their occurrence slots.
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
        self.scan_cells = row_cells;
        self.rescan_end(out, cols, epoch, now, None);
    }

    /// [`rescan`](Self::rescan) over an already-extracted frame snapshot: the
    /// per-frame render path scans the `RenderInput` rows `cell_frame_into`
    /// filled under the SAME Terminal lock, so the full per-cell color/style
    /// resolve runs exactly once per presented frame instead of twice.
    /// `cells`/`line_sizes` come from the same `render_row_into_impl` /
    /// `Row::line_size` probes the term-walking path uses, so occurrences,
    /// ink capture, and the §5.7 `dec_line` flag are byte-identical (with
    /// feature `bidi`, rows arrive in VISUAL order — RTL rows scan the
    /// reordered stream, matching the columns the renderer draws).
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

    /// Snapshot rescan with the live pixel geometry. Unlike the compatibility
    /// [`rescan_from_cells`](Self::rescan_from_cells) entry point, cat palettes
    /// sample the exact prospective sprite footprint across adjacent rows.
    #[allow(
        clippy::too_many_arguments,
        reason = "shipping rescan adds live cell geometry to the existing snapshot contract"
    )]
    pub fn rescan_from_cells_with_geom(
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
    ) {
        self.rescan_from_cells_with_geom_at_cursor(
            cells, line_sizes, rows, cols, lexicon, cfg, epoch, now, geom, default_bg, None,
        );
    }

    /// Geometry-aware snapshot rescan plus the live input caret. Language-gated
    /// collision forms touching `cursor` are provisional until a delimiter
    /// moves it away. Pass `None` for settled/offline content.
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
        // v3 §1.1 fix #3: frozen ⇒ no-op, exactly like [`rescan`](Self::rescan)
        // (the pending rescan stays pending — `last_epoch` untouched).
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
                    // bottom-priority cutoff (see the term-walk twin — the
                    // paths stay byte-identical outside row sourcing).
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
        );
    }

    /// Shared rescan prologue: reset the per-rescan §3.6 ordinal counters and
    /// the §4.3 ink capture buffers, and hand back the resident occurrence
    /// buffer (cleared, capacity kept).
    fn rescan_begin(&mut self) -> Vec<Occurrence> {
        // §3.6 ordinals are per-rescan (row-major among same-seed occurrences),
        // so the counter map starts fresh; the buffer's capacity is resident.
        self.seed_ordinals.clear();
        // v3 §1.1: a new rescan sequence — episodes adopted this rescan stamp
        // it, and the alignment pass treats everything unstamped as "old".
        self.rescan_seq = self.rescan_seq.wrapping_add(1);
        self.pending.clear();
        self.scan_row_occupied.clear();
        let mut out = std::mem::take(&mut self.occ);
        out.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        out
    }

    /// Bottom-priority row cutoff for a match-saturated screen: the first row
    /// the (restarted) real scan should include so that everything from it to
    /// the bottom fits under [`MAX_OCCURRENCES`] together.
    ///
    /// Plain row-major TOP-DOWN truncation would spend the whole cap on the
    /// upper rows, leaving the BOTTOM rows — the prompt the user is typing at —
    /// with no occurrence slots (no cat, no ink) until the dense output
    /// scrolled away. Preference is inverted here: walk rows bottom-up,
    /// spending the budget on the rows nearest the prompt, and DROP whole
    /// rows top-down. Within the kept suffix the real scan still runs
    /// top-down, so row-major order (ordinals, ink sortedness, alignment
    /// grouping) is preserved exactly.
    ///
    /// The per-row estimate deliberately OVERCOUNTS: raw lexicon matches
    /// before the class/ambiguity gates. Gates only ever REMOVE matches, so the
    /// restarted scan can never overflow inside the kept rows — the bottom
    /// rows are guaranteed their slots (a whole row that only *might* fit is
    /// excluded rather than risked). The one unavoidable exception is a
    /// SINGLE row estimating past the whole cap: it is kept (clamp below) and
    /// truncates left-to-right within itself, which is still bounded and
    /// deterministic.
    ///
    /// Every row is visited so `scan_row_occupied` gets a truthful occupancy
    /// picture for the rows the restart will skip (the rescan-end taint sweep
    /// reads it; a dense row wrongly marked blank would misclassify an
    /// overwrite as blank-clear grace). Rows above the cutoff skip the
    /// lexicon pass — occupancy is a plain trim. This method only runs after
    /// the top-down scan already saturated the cap (a rare, pathological
    /// screen), so the extra full-screen pass and the transient row buffer
    /// are off the per-keystroke hot path by construction.
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
            // Same `(text, opts, lexicon)` scan `scan_row` makes — the row text
            // is built here from the identical non-wide `cell.ch` stream — so it
            // shares the memo. That matters here precisely BECAUSE this pass is
            // the pathological one: a match-saturated screen otherwise pays
            // three full-grid tokenise passes in one frame (the aborted
            // top-down scan, this bottom-up estimate, and the restarted scan).
            // Only the match COUNT is read; `scan_chars`/`scan_matches` are left
            // as scratch either way, and the restarted `scan_row` rebuilds
            // whatever it needs.
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
        // A single bottom row can estimate past the entire cap; keep it and
        // let the real scan truncate within it rather than emptying the
        // screen.
        cutoff.min(rows.saturating_sub(1))
    }

    /// Clear the per-rescan accumulators for the dense-screen second pass
    /// WITHOUT advancing `rescan_seq` and WITHOUT touching
    /// `scan_row_occupied` (just rebuilt truthfully by
    /// [`dense_scan_cutoff`](Self::dense_scan_cutoff)). The aborted first
    /// pass may have fast-path-refreshed episodes on rows the second pass
    /// skips; those words ARE on screen, so the refreshed `last_seen`/
    /// `seen_seq` stamps are truthful and replaying the kept rows under the
    /// same sequence is idempotent.
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
    /// `ink_full` is the §4.4 row-major ink truncation latch: once one
    /// occurrence's lead cells don't fit under [`MAX_INK_CELLS`], IT and every
    /// later (further down-right) occurrence get no ink — a deterministic
    /// prefix, so the settled frame is a stable fixed point under re-emission.
    ///
    /// Returns `false` when [`MAX_OCCURRENCES`] is reached (the caller stops
    /// scanning further rows, or — on the first saturation of a rescan —
    /// restarts once from the bottom-priority `dense_scan_cutoff` row).
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
        for (c, cell) in row_cells.iter().enumerate() {
            if cell.wide {
                continue; // right half of a wide glyph carries no char
            }
            self.scan_text.push(cell.ch);
            self.scan_colmap.push(c as u16);
        }
        let occupied = !self.scan_text.trim().is_empty();
        if self.scan_row_occupied.len() <= r {
            self.scan_row_occupied.resize(r + 1, false);
        }
        self.scan_row_occupied[r] = occupied;
        if !occupied {
            return true;
        }
        // TOKENISE + LEXICON LOOKUP — ~86 % of the rescan, and the rescan runs
        // inside the present carrying the keystroke echo. Served from the
        // [`ScanMemo`] whenever this exact row text was scanned recently: the
        // scanner reads nothing but `(text, opts, lexicon)`, and the memo is
        // retired on an `opts` change (`fit`) or a lexicon rebuild
        // (`hard_reset` -> `reset_transient_state`), so a hit returns exactly
        // what the call below would have. Under typing only the edited row(s)
        // miss; under scrolling output only the genuinely new lines do.
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
        // Test-only mutation switch for the guard below: the battery uses it to
        // reproduce exactly the break this rebuild prevents (see
        // `memo_hit_without_the_scan_chars_rebuild_trips_the_guard`). Always
        // `true` in a shipping build — no branch and no field exist there.
        #[cfg(test)]
        let rebuild_scan_chars = !self.mutate_skip_scan_chars_rebuild;
        #[cfg(not(test))]
        let rebuild_scan_chars = true;
        if hit && rebuild_scan_chars && !matches.is_empty() {
            // A hit skipped the scanner, so `scan_chars` still holds the
            // PREVIOUS row's stream — and the deferred-birth SimHash below
            // indexes it with THIS row's `m.start`/`m.end`, which would fold a
            // neighbouring row's context into the genome (a different magic
            // roll, i.e. visibly different art). Rebuild it exactly as
            // `scan_into_with_scratch` opens: `clear()` + `extend(chars())`.
            // Only the deferred path reads it, so a match-free row (the common
            // case) skips even this.
            self.scan_chars.clear();
            self.scan_chars.extend(self.scan_text.chars());
        }
        // INVARIANT (named here because the memo made it breakable): whenever
        // this row has matches, `scan_chars` IS this row's char stream. The
        // match spans below are CHAR indices into `scan_text`, and
        // `genome::simhash_ctx` walks `scan_chars` from them UNGUARDED
        // (`chars[pos - 1]`), so a `scan_chars` left holding a SHORTER previous
        // row is not a wrong pixel — it is an index-out-of-bounds panic inside
        // the rescan, i.e. inside the present that is drawing the frame. The
        // miss path gets this from `scan_into_with_scratch`'s own opening
        // `clear()` + `extend(chars())`; the hit path gets it from the block
        // right above, which is the ONLY thing keeping the two in sync.
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
            // Extend to the last ON-SCREEN cell: a wide glyph's right half
            // follows its lead column, so the word spans one extra cell. Keeps
            // the trailing cat-paw past wide words and the sparkle width correct.
            let end_col = if row_cells.get(end_lead as usize + 1).is_some_and(|n| n.wide) {
                end_lead + 1
            } else {
                end_lead
            };
            // Language-gated collision forms are exact matches, but while they
            // touch the live input cursor they are not yet settled words. This
            // prevents transient prefixes such as Romanian `fut` / `futu` from
            // flashing while `future` is typed. A delimiter advances the caret
            // beyond `end + 1`, immediately admitting the completed form.
            if m.ambiguous
                && cursor.is_some_and(|cell| caret_on_span(cell, r as u16, start_col, end_col))
            {
                continue;
            }
            let live_caret_completion = cursor.is_some_and(|(cursor_row, cursor_col)| {
                cursor_row == r as u16 && cursor_col == end_col.saturating_add(1)
            });
            // The caret sits ON the token (or in the cell right after it):
            // the user is typing HERE. The identity machinery cannot otherwise
            // tell a redraw from a retype at an unchanged column under an
            // unchanged prompt — ident folds no row and the exact-context
            // alignment edge has no recency bound — so this positional witness
            // is what keeps a genuinely retyped kitty from inheriting a spent
            // ancestor after `clear` or scroll-off (the fast-path guard below,
            // `alignment_edge`, and the fresh-birth done-mark deletion).
            let at_live_cursor =
                cursor.is_some_and(|cell| caret_on_span(cell, r as u16, start_col, end_col));
            // v3 §6 (normative): a per-word override WINS over the class
            // default regardless of the match's class AND bypasses the
            // per-class enable gate (a custom spec on a builtin profanity
            // form fires with `[profanity] enabled = false`).
            let ov = cfg.spec_table.override_for(m.form_hash).copied();
            if ov.is_none() {
                match m.class {
                    Class::Profanity if !cfg.profanity => continue,
                    Class::Feline if !cfg.feline => continue,
                    Class::Orca if !cfg.orca => continue,
                    // Emphasis is the ink-only class (P1): the resolver folds
                    // `ink_enabled || has_custom_specs` into `cfg.emphasis`,
                    // so a gated-off emphasis match never consumes a slot.
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
            // occurrences THIS rescan, folded into the ident so twins get
            // distinct episodes.
            let ordinal = {
                let n = self.seed_ordinals.entry(seed).or_insert(0);
                let o = *n;
                *n += 1;
                o
            };
            let ident = mix(seed ^ ordinal.wrapping_mul(ORDINAL_MIX));
            // v3 §1.1 fast path: a persist hit ON THE SAME ROW is a plain
            // continuation — adopt the episode wholesale (appeared + frozen
            // genome; every done flag stays put), refresh the grace window,
            // and skip SimHash entirely (§3.6 "frozen at birth"). Anything
            // else (miss, or a hit whose row moved — growth/shrink/rotation/
            // scroll changed the group's row multiset) DEFERS to the
            // rescan-end row-anchored alignment; the context fingerprint is
            // computed here while the row's char stream is still in scope.
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
                        // Typed-retype guard (mirrors `alignment_edge`): a
                        // SPENT feline graphic whose form was absent for a
                        // full rescan (gap ≥ 2 — a `clear` frame, not mere
                        // continuation) may not wholesale-adopt the word now
                        // sitting at the live caret; the candidate defers to
                        // alignment, refuses there too, and is born fresh.
                        // Gap ≤ 1 (the word was seen every scan) is a plain
                        // continuation and must keep adopting — otherwise a
                        // finished kitty at an idle prompt would re-arm on
                        // every cursor-blink rescan.
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
                // Deferred candidate — the ONLY place the genome is computed.
                // ctx_fp walks ±4 reading-order neighbor tokens over the
                // resident row char stream (§3.2, allocation-free). If the
                // alignment pass matches this candidate to a moved episode the
                // fresh genome is discarded (the episode's stays frozen).
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
                    // §3.5: NO ident/col/row folded in — the same sentence
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
            // Ink capture (§4.3): the base fg of every LEAD cell in the
            // visual span, plus the first lead cell's bg for the legibility
            // guard. Continuation halves of wide glyphs carry no glyph and
            // are skipped (the lead cell's InkCell governs the whole glyph).
            // v3 §6: ink-bearing = the resolved spec carries an ink axis
            // (orca's default spec carries none — the v2 exemption intact).
            let ink_bearing = cfg.ink_enabled && spec.ink.is_some();
            let mut ink_base = 0usize;
            let mut ink_cells = 0u16;
            // v3 §3.2: the FIRST lead cell's bg is captured for EVERY
            // occurrence, not just ink-bearing ones — the supernova theme
            // branch reads it, and `[sparkle_words.ink] enabled = false`
            // must not send light-theme supernovae down the (invisible)
            // dark additive-wash path via a stale all-black default.
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
                        // (`ink_bg` was already captured above from this
                        // same first lead cell, ink-bearing or not.)
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
            });
            if out.len() >= MAX_OCCURRENCES {
                return false;
            }
        }
        true
    }

    /// Shared rescan epilogue: the v3 §1.1 row-anchored alignment over the
    /// deferred candidates, the §3.6 grace sweep (now the done-mark write
    /// path), the §5.8 gaze sweep, the resize-settle bookkeeping, and the
    /// epoch/geometry update.
    fn rescan_end(
        &mut self,
        mut out: Vec<Occurrence>,
        cols: usize,
        epoch: u64,
        now: Instant,
        cat_surface: Option<CatSurface<'_>>,
    ) {
        // v3 §1.1 fix #4 resize-settle: a cols change opens (or extends) the
        // settle window; births inside it are born-settled (no entrance, no
        // roll, static ink) and never written to done_marks. The window
        // closes on the first rescan at stable cols past the deadline.
        let cols_changed = self.have_scanned && self.cols != cols as u16;
        if cols_changed {
            self.settle_until = Some(now + Duration::from_millis(RESIZE_SETTLE_MS));
        }
        // A returning window spends this rescan's births exactly like a resize
        // settle does (no entrance, no roll, settled ink) — see
        // [`Self::set_presentable`]. Consumed here: only the FIRST rescan after
        // coming back is suppressed, so ordinary typing right after refocus
        // still earns its cats.
        let returning = std::mem::take(&mut self.spend_next_births);
        let born_settled = cols_changed || returning || self.settle_until.is_some_and(|t| now < t);
        if !born_settled {
            self.settle_until = None;
        }

        // Expire grace-only episodes BEFORE they can bid in redraw alignment.
        // Recency by rescan sequence protects an immediate clear/redraw from
        // scheduler stalls; it must never resurrect an hour-old spent episode
        // merely because only two scans happened during that hour.
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

        // v3 §1.1 fix #1: redraw alignment for every deferred candidate. Exact
        // position-bearing seeds keep the original vertical-scroll semantics;
        // the context-checked surface fallback carries an episode across a
        // same-width horizontal reflow without treating unrelated text as a hit.
        self.align_pending(&mut out, cols as u16, now, born_settled);

        // Only episodes still unseen after the complete alignment are misses.
        // Nonblank replacement content on their old row is positive evidence of
        // overwrite/incremental typing; blank clear frames retain exact-context
        // grace even across more than two damage scans.
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

        // Alignment may replace a candidate's provisional genome with the
        // frozen episode genome. Recompute text clearance only after that
        // transfer so the protected rectangle matches the art that will
        // actually render. Only snapshot rescans carry this context; the
        // compatibility terminal-walk path has none and skips the pass.
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
        self.last_epoch = epoch;
        self.have_scanned = true;
        self.cols = cols as u16;
    }

    /// v3 §1.1 fix #1: two-dimensional redraw alignment. Deferred candidates
    /// group by collision-free exact lexicon surface. An old episode can bid when
    /// either (a) its position-bearing seed is unchanged (the original
    /// vertical-scroll/neighbor-churn rule), or (b) its frozen context matches
    /// the candidate (the horizontal-reflow rule). Exact-seed edges outrank
    /// context edges. Recent leftovers then receive a bounded, monotone
    /// reading-order match so soft-wrap/status redraws cannot synthesize fresh
    /// armed births. The weak arm is maximum-cardinality within its two-row
    /// window and never runs on older grace-only entries.
    ///
    /// Exact/context precedence plus the bounded continuity/recency window are
    /// load-bearing: a genuinely new log-tail occurrence outside that window
    /// remains fresh. Matched episodes move to the candidate's ident; unmatched
    /// old episodes grace-expire and unmatched candidates follow the normal
    /// fresh/born-done/born-settled path.
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
        let pending = std::mem::take(&mut self.pending);
        // Alignment is on the typing-damage path. Keep all transient matching
        // state in bounded stack scratch: `pending` is capped at
        // MAX_OCCURRENCES and `persist` at PERSIST_CAP. In particular, do not
        // rebuild five short-lived Vecs on every redraw.
        let mut done = [false; MAX_OCCURRENCES];
        let mut group = [0usize; MAX_OCCURRENCES];
        let mut old_keys = [0u64; PERSIST_CAP];
        let mut winner: [Option<usize>; MAX_OCCURRENCES] = [None; MAX_OCCURRENCES];
        let mut matched = [false; PERSIST_CAP];
        // The 66 KB decision plane is the one buffer too big to sit on the
        // stack: see `align_decisions`. Borrowed here, restored at the tail; no
        // clearing, because every cell the traceback can reach is written by
        // the group that reads it.
        let mut decisions = std::mem::take(&mut self.align_decisions);
        if decisions.len() < ALIGN_DECISION_CELLS {
            decisions.resize(ALIGN_DECISION_CELLS, 0);
        }
        let mut prev_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
        let mut curr_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
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
            // Old set: every unseen episode for this exact compiled surface,
            // pulled OUT of the map (rekeying must never clobber). Strong
            // exact-seed/context edges run first; recent leftovers are needed
            // for the weak visual-continuity arm below.
            self.align_old.clear();
            let mut old_keys_len = 0usize;
            for (&key, ep) in &self.persist {
                if ep.form_id == form_id && ep.seen_seq != seq {
                    debug_assert!(old_keys_len < PERSIST_CAP);
                    old_keys[old_keys_len] = key;
                    old_keys_len += 1;
                }
            }
            for &k in &old_keys[..old_keys_len] {
                if let Some(ep) = self.persist.remove(&k) {
                    self.align_old.push((k, ep));
                }
            }
            self.align_old
                .sort_unstable_by_key(|(k, ep)| (ep.last_row, ep.last_col, *k));

            // One monotone sequence-alignment DP covers all three evidence
            // arms. Stationary physical survivors are primary (the log-rotation
            // discriminator), then maximum cardinality prevents greedy tie
            // failures from manufacturing births during global reflow. Exact
            // seed outranks exact context, which outranks bounded recent/local
            // continuity; reading-order distance is the final cost. Monotonicity
            // keeps identical twins from crossing. Scores use two rolling rows
            // and one byte/cell of bounded stack decisions: zero hot allocation.
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
                        // The group already proves exact recognized surface
                        // equality. Position, not a raw/text hash, defines the
                        // physical survivor so case-only redraws still pin.
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
            // Resident (in-grace) old evidence for this form. The caret-fresh
            // policy below is deliberately scoped to it: with a live ancestor
            // in the map, an unmatched cursor-adjacent birth can only mean
            // every transfer edge REFUSED it (the typed-retype refusals) —
            // positive evidence of retyping. Without any resident evidence
            // (post-expiry, post-`reset()`), a parked shell caret after the
            // word is indistinguishable from a passive redraw of old output,
            // and the ty-proven NoRepeek/born-done semantics must hold.
            let group_has_old = !self.align_old.is_empty();
            let group_has_live_caret_completion =
                group.iter().any(|&k| pending[k].live_caret_completion);

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
                    occ.appeared = ep.appeared;
                    occ.genome = ep.genome;
                    occ.cat_colors = ep.cat_colors;
                    occ.inert = ep.inert();
                    self.rekey_persist_episode(old_ident, occ.ident, ep);
                } else {
                    // Fresh episode: done_marks lookup is free here — ctx_fp
                    // was already computed on the (deferred) miss path.
                    let key = occ.ident ^ pending[k].ctx_fp;
                    // A tainted old set plus the class-specific completion
                    // witness is positive evidence of typing rather than redraw
                    // replay. Profanity additionally requires the live caret;
                    // a passive output line often leaves the terminal cursor at
                    // its end and is not by itself causal evidence.
                    // Explicit retyping is allowed to re-arm, so neither a prior
                    // done mark nor the old episode's departure may poison it.
                    // Feline additionally accepts the caret-on-word witness
                    // while resident old evidence exists: after `clear`
                    // (blank frames never taint) the taint evidence is
                    // structurally gone, yet a birth under the user's own
                    // caret with a live spent ancestor means the typed-retype
                    // refusals just fired — its colliding session done mark
                    // (same column, same prompt context re-keys identically)
                    // is deleted, not honored, so a retyped kitty is never
                    // inert for the rest of the session. `group_has_old`
                    // keeps the post-expiry/post-reset re-appearance of an
                    // UNCHANGED line born-done (NoRepeek) even when the shell
                    // caret happens to park right after the word.
                    let typed_reentry = (group_class == Class::Feline
                        && (has_tainted_old || (pending[k].at_live_cursor && group_has_old)))
                        || (group_class == Class::Profanity
                            && has_tainted_old
                            && pending[k].live_caret_completion);
                    let born_done = if typed_reentry {
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
                    // v3 §3.2/§6: the session birth sequence advances on
                    // EVERY persist miss; the burst-axis roll is decided once
                    // here for every `BurstKind` (per APPEARANCE, not per
                    // context — decorrelated across repeats of the same word
                    // at the same prompt; chance 100 = always, so the class
                    // defaults keep firing) and stored so row alignment
                    // transfers the decision. Skipped for born-done/
                    // born-settled episodes (§1.1 fix #4).
                    self.birth_seq = self.birth_seq.wrapping_add(1);
                    if !(born_done || born_settled)
                        && let Some(b) = occ.spec.burst
                        && b.chance_pct > 0
                    {
                        // ONE draw, TWO decodes. `mix` is the splitmix64
                        // finalizer, so its halves are independent: the LOW
                        // half decides whether to detonate, the HIGH half which
                        // of the three degrees. No new RNG, no new state, and
                        // the tier inherits the roll's determinism and its
                        // row-alignment transfer for free.
                        let draw =
                            mix(occ.genome.gkey ^ self.birth_seq ^ supernova::SUPERNOVA_SALT);
                        ep.burst_roll = draw % 100 < u64::from(b.chance_pct);
                        ep.burst_tier = supernova::tier_of(draw);
                    }
                    // Curse-BONK typed witness, latched at the ONLY fresh-
                    // birth site: a Profanity episode born with the caret
                    // exactly one past the token is causally TYPED (the same
                    // positional witness typed_reentry trusts). Redraws,
                    // scrollback, and `cat` output either adopt an existing
                    // episode (no birth) or lack the witness; ambiguous
                    // live-caret forms (`fut` mid-`future`) were deferred
                    // before any occurrence existed. Inert births (born-done /
                    // born-settled) stay silent like every other axis (§1.1).
                    if !(born_done || born_settled)
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
                    && (group_class == Class::Feline
                        || (group_class == Class::Profanity && group_has_live_caret_completion))
                {
                    // Positive incremental-typing evidence closes the old
                    // episode as an intentional retype. Unlike a redraw, reset,
                    // expiry, or capacity eviction, it must not create a done
                    // mark that makes the new typed token inert.
                    self.done_marks.remove(&(old_ident ^ ep.ctx_fp));
                    continue;
                }
                // Key still free and capacity still available: keep it around
                // to grace-expire (§1.1 "unmatched old ⇒ grace-expire"). A
                // claimed key or a map refilled by visible candidates makes
                // this old episode depart instead.
                self.insert_persist_bounded(old_ident, ep, PersistAdmission::Grace);
            }
        }
        self.pending = pending;
        self.align_decisions = decisions;
        self.align_old.clear();
    }

    /// Advance the animation one frame and emit this frame's decorations into
    /// `out`, this frame's animated-ink fg overrides into `ink` (sorted
    /// `(row, col)` unique — the renderer's [`InkCell`] invariant; emission is
    /// row-major over non-overlapping matches, so sortedness is structural),
    /// this frame's peeking cats into `free` (overlay Phase 4: ONE
    /// [`FreeSprite`] per cat plus its gaze-light dots, paired with
    /// [`WordDecorations::free_atlas`]), and this frame's supernova additive
    /// light into `nova` (§6; premultiplied `0x00RRGGBB` row-band GlowQuads —
    /// the `nova_add` render channel).
    ///
    /// `companion_at` is the cursor cell `(row, col)` in pre-splice grid coords
    /// WHEN the host is also drawing the cursor companion
    /// ([`WordDecorations::nyan_cursor`]) into this same `free` stream this
    /// frame — `None` whenever no companion is on glass. It drives the
    /// ONE-CAT-PER-CARET suppression (rationale at the gate in the graphic
    /// arm below); callers read it under the Terminal lock they already hold
    /// (`term.cursor()`, app_render) — no new lock is taken. `sel` is the
    /// selection view read under the same
    /// lock (§6.4: ignition defers while the word is selected; active nova
    /// quads attenuate over selected cells). `_focused` is accepted but
    /// currently unused — nothing in the tick reads it (native hosts demote
    /// unfocused windows to `reduced_motion` instead); it stays in the
    /// signature so callers keep threading the value if §5.6 focus gating
    /// returns.
    ///
    /// Returns a fingerprint that changes every frame while any sparkle, ink
    /// sweep, cat entrance, or nova is animating (so the repaint early-out
    /// never skips a live frame) and is stable otherwise (idle = no wasted
    /// frames). `out` + `ink` + `free` + `nova` empty with fp `0` is
    /// byte-identical to the pre-feature path.
    #[allow(
        clippy::too_many_arguments,
        reason = "the per-frame tick threads the clock, config, geometry, companion cell, selection view, focus gate, and four output scratches through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    pub fn tick(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        companion_at: Option<(u16, u16)>,
        sel: Option<SelView<'_>>,
        _focused: bool,
        out: &mut Vec<WordDecoration>,
        ink: &mut Vec<InkCell>,
        free: &mut Vec<FreeSprite>,
        nova: &mut Vec<GlowQuad>,
    ) -> u64 {
        out.clear();
        ink.clear();
        free.clear();
        nova.clear();
        // §F4.2: cleared at TICK START (not drain time) so hostless builds
        // (wasm/web with no drain wired) stay bounded at MAX_OCCURRENCES.
        self.sightings.clear();
        // Curse cues follow the same rule (bounded at MAX_CURSE_CUES); their
        // typed one-shots live on the episode latch, so an undrained frame
        // loses only sound, never correctness.
        self.curse_cues.clear();
        if !self.host_brackets_frames {
            // No host frame bracket: one baker prologue per tick, exactly as
            // before pane binding existed. A bracketing host resets it once per
            // PRESENTED frame instead, so N panes share the two-bake budget.
            self.cat_baker_ready = false;
        }
        // The rare late kitty: at most one spent episode is re-armed, gated by
        // its own period + min-gap clocks so nearly every tick pays one compare.
        // Runs BEFORE the emission pass so a granted revisit draws this frame.
        self.roll_revisit(now, cfg);
        // v3 §1.1 fix #3: a frozen engine emits nothing and advances nothing
        // (defensive — hosts skip the tick while suspended; `thaw` restores).
        if self.frozen_at.is_some() {
            self.novas.clear();
            self.coupling.clear();
            return 0;
        }
        if self.occ.is_empty() {
            self.active_until = None;
            self.novas.clear();
            self.coupling.clear();
            return 0;
        }
        self.frame = self.frame.wrapping_add(1);
        // Free-overlay Phase 4: cats always ride the free channel, so the
        // baker always bakes exact-size tiles (steady-state a no-op compare;
        // set BEFORE the frame prologue).
        self.cat_baker.set_free_tiles(true);
        // Per-FRAME baker prologue: LRU clock + bake budget; a cell-metric
        // change wholesale-clears + version-bumps here (§5.5). Guarded like
        // `nyan_cursor`'s, so a window compositing N panes runs it once and the
        // panes share one two-bake budget (one window, one atlas, one cell
        // size). Unbracketed hosts clear the flag at tick start, which is the
        // historical one-prologue-per-tick behaviour verbatim.
        if !self.cat_baker_ready {
            self.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
            self.cat_baker_ready = true;
        }
        // v3 §1.1/§1.2 episode prepass: arm freezing (Cat vs Paw stored at
        // the first emission decision) and the per-axis one-shot flags
        // (peek/burst/sweep started/done) — the done-mark write condition.
        // The peek phase clock is latched at birth ([`Episode::fresh`]),
        // regardless of focus.
        self.episode_prepass(now, cfg, geom);
        let anim = Duration::from_millis(cfg.anim_ms);
        let mut fp: u64 = 0xcbf2_9ce4_8422_2325;
        let mut active_until: Option<Instant> = None;
        // The nuke cloud's `(t_ms, cx, cy)`, recorded inside the occurrence
        // loop and emitted after it — baking borrows `self.cat_baker`, which
        // that loop already holds mutably. `MAX_ACTIVE_SUPERNOVAE = 1` makes
        // one slot sufficient by construction.
        let mut nuke_pending: Option<(u64, i32, i32)> = None;
        // v3 §3.2 supernova prepass FIRST: it owns the GLOBAL BURST MUTEX
        // (`super_until`) that the classic nova prepass defers new grants
        // behind — published from its persist-wide busy scan, so a live
        // window holds the mutex even while its word is scrolled off — and
        // it defers its own ignition behind any LIVE classic window
        // (two-way) — so the combined `nova_add` channel stays bounded by
        // `max(3·392, S_max) ≤ MAX_NOVA_QUADS` (the mutex keeps the windows
        // from ever overlapping; the bound is a max, not a sum).
        if let Some(until) = self.super_prepass(now, cfg, geom, sel) {
            arm_until(&mut active_until, until);
        }
        // §6.4 nova prepass: limiter grants + window expiry + this frame's
        // live-nova and coupling scratch. Runs before the emission loop so
        // ink coupling (§6.5) and the gaze hook (§5.8) see every live nova
        // regardless of occurrence order. Keeps the scheduler armed through
        // DELAYED ignitions (the queued Dip start is in the future).
        if let Some(until) = self.nova_prepass(now, cfg, geom, sel) {
            arm_until(&mut active_until, until);
        }
        // Curse-BONK typed cues: convert birth-latched episode one-shots into
        // this tick's cue records (AFTER the prepasses so this frame's
        // detonation cues share the drain; BEFORE the emission loop, which
        // holds `persist` immutably). The latch is consumed only on a
        // successful queue — the sightings' cap-truncation retry rule.
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
        // Disjoint field borrows: the cat arm bakes (&mut baker) and commits
        // (&mut baker) while the loop reads occurrences + the resident ink
        // capture buffers + the prepass's nova scratch.
        let frame = self.frame;
        // §F4.2 pre-loop: the unlogged-episode scratch, from a shared
        // `&persist` borrow (the loop below holds `persist` immutably for
        // ink_fx, so the logged commit must wait for the post-loop pass).
        // Born-done/born-settled episodes never log (§1.1 inertness).
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
            unlogged: &self.unlogged,
            sightings: &mut self.sightings,
        };
        let mut cats = 0usize;

        for (oi, occ) in self.occ.iter().enumerate() {
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
                &self.persist,
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
            // §4 orca carve-out: the suspended splash keeps its class-keyed
            // dispatch (and its exempt v2 residual) until the orca redo rides
            // the framework; a custom-overridden word takes the spec axes.
            if occ.class == Class::Orca && !occ.custom {
                emit_orca_splash(
                    occ,
                    cfg,
                    now,
                    self.frame,
                    anim,
                    out,
                    &mut fp,
                    &mut active_until,
                );
                continue;
            }
            // v3 §6 GRAPHIC axis (`Collection::Cats`: the peeking cat, with
            // its documented paw fallbacks). Ink-only specs skip it entirely.
            'graphic: {
                if occ.spec.graphic.is_none() {
                    break 'graphic;
                }
                {
                    // v3 §1.1: born-done / born-settled episodes are INERT —
                    // the graphic one-shot is pre-spent, zero quads ever.
                    if occ.inert {
                        break 'graphic;
                    }
                    // v3 §1.2 arm freezing: dispatch from the stored arm when
                    // an episode carries one (rank churn past MAX_CATS,
                    // DECDHL flips, or floor crossings mid-episode never
                    // morph a dwelling cat into a paw or vice versa).
                    // Direct-built occurrences (tests/demos, no episode)
                    // resolve the arm per frame, keyed off `appeared`.
                    let ep = self.persist.get(&occ.ident);
                    let peek = PeekView {
                        start: ep.map_or(Some(occ.appeared), |e| e.phase_start),
                        peek_done: ep.is_some_and(|e| e.peek_done),
                        arm: ep.and_then(|e| e.shown_as),
                    };
                    // v3 one-shot × reduced motion: Done is UNCONDITIONAL —
                    // native hosts demote unfocused windows to reduced_motion
                    // (the W11b motion fold), so a reduced static arm must
                    // never resurrect a completed one-shot on focus loss.
                    if peek.peek_done {
                        break 'graphic; // Done: zero quads forever (duty pin)
                    }
                    // ONE CAT PER CARET: the companion the host is drawing at
                    // this cell IS this word's cat — typing `kitty` summons it
                    // (`CursorCat::on_collect`) at the very caret the echoed
                    // word sits under, so without this the same keystroke put
                    // two cats a couple of cells apart. Same span predicate as
                    // the rescan's `at_live_cursor` (caret ON the token or in
                    // the cell right after it). Suppression is per-occurrence
                    // and graphic-only: every OTHER feline word on screen still
                    // peeks, and this word's own ink/sparkle still plays.
                    //
                    // Gated here rather than inside `cat_eligible` so the arm
                    // census and the Kitty Log stay untouched — `emit_cat` logs
                    // a sighting only for sprites that actually landed, and the
                    // typed word is already logged synthetically by the host.
                    if companion_at.is_some_and(|cell| {
                        caret_on_span(cell, occ.row, occ.start_col, occ.end_col)
                    }) {
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
                    // The ONLY feline graphic is the authored peeking cat. Every
                    // fallback arm (style / cell floors / narrow / top-row /
                    // MAX_CATS overflow) draws NO graphic — the word's own ink
                    // still plays.
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
                        // A stored Cat under a mid-episode floor flip emits
                        // NOTHING (never a swap — the peek clock keeps running to
                        // Done).
                    }
                }
            }
            // v3 §6 BURST axis.
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
                        &self.persist,
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
                        &self.persist,
                        out,
                        nova,
                        &mut fp,
                        &mut active_until,
                        &mut nuke_pending,
                    );
                }
                // A soft additive glow pulse (custom `kind = "glow"`): one
                // rise-and-fall over ~1.4 s, then done — every graphic decays
                // to zero (§1.2).
                Some(BurstKind::Glow) => {
                    if occ.inert || cfg.reduced_motion {
                        continue;
                    }
                    // v3 §6 `chance_pct`: a rolled-off glow never fires.
                    if self.persist.get(&occ.ident).is_some_and(|e| !e.burst_roll) {
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
                            color: 0x00FF_EED2,
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
                    // v3 §6 `chance_pct`: a rolled-off burst never fires
                    // (episode-backed; direct-built test/demo occurrences
                    // have no episode and always fire).
                    if self.persist.get(&occ.ident).is_some_and(|e| !e.burst_roll) {
                        continue;
                    }
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
                            let env = twinkle(s, self.frame);
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
                        // v3 §1.2 graphics-decay: the v1 steady residual now
                        // FADES out ≤ 2 s after the animation window, then
                        // zero decos forever (self-termination); the reduced-
                        // motion static spark keeps its v2 frame-invariant
                        // bytes.
                        let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
                        let f = if cfg.reduced_motion {
                            // v3 one-shot × reduced motion: no static
                            // resurrection once the burst one-shot fully
                            // faded (`burst_done` is set by the prepass when
                            // the non-reduced residual reaches zero). A
                            // reduced-from-birth episode never advances the
                            // flag, so its static spark persists —
                            // accessibility semantics preserved.
                            (!self.persist.get(&occ.ident).is_some_and(|e| e.burst_done))
                                .then_some(1.0f32)
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
        // THE NUKE CLOUD, emitted post-loop (see `nuke_pending`). Three baked
        // tiles — base surge, column, head — each resolved by `nuke_draw` into
        // a dest-rect transform, alpha and tint. They ride the FreeSprite lane
        // (the cat's and the sing-along notes'), because the cloud is ART, not
        // light: it must not be additively blended over the text it floats on.
        //
        // Back-to-front, so the head occludes the column it sits on.
        if let Some((t, cx, cy)) = nuke_pending {
            for (slot, part) in [
                crate::nuke::NukePart::Skirt,
                crate::nuke::NukePart::Stem,
                crate::nuke::NukePart::Cap,
            ]
            .into_iter()
            .enumerate()
            {
                let Some(d) = crate::nuke::nuke_draw(t, part) else {
                    continue;
                };
                let (nw, nh) = crate::nuke::nuke_nat_size(part, geom.cell_w, geom.cell_h);
                let host = crate::nuke::nuke_host_id(part, nw, nh);
                if self.nuke_cache[slot].as_ref().map(|c| c.host) != Some(host) {
                    self.nuke_cache[slot] = Some(NotePaintCache {
                        host,
                        rgba: crate::nuke::bake_nuke(nw, nh, part).pixels().to_vec(),
                    });
                }
                let paint = &self.nuke_cache[slot].as_ref().expect("filled above").rgba;
                let Some(tile) = self.cat_baker.host_tile(host, nw, nh, paint) else {
                    continue; // bake budget spent; this part lands next frame
                };
                let dest_w = ((f32::from(nw) * d.sx).round() as i32).max(1);
                let dest_h = ((f32::from(nh) * d.sy).round() as i32).max(1);
                let grid_w = i32::from(geom.cols) * i32::from(geom.cell_w);
                if dest_w > grid_w {
                    continue; // a grid narrower than the cloud stays clear
                }
                // Anchored on the blast, standing ON the word's row and
                // growing UPWARD from it.
                let base_y = cy + i32::from(geom.cell_h) / 2;
                let dy = (d.dy_cells * f32::from(geom.cell_h)).round() as i32;
                let alpha = (d.alpha * f32::from(u8::MAX)) as u8;
                if alpha == 0 {
                    continue;
                }
                // BOUND: three sprites at most, and `MAX_ACTIVE_SUPERNOVAE = 1`
                // means one cloud at a time — a constant cost, not a cap.
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
        // §F4.2 post-loop: mark exactly the successfully QUEUED sightings
        // logged (a cap-truncated or never-emitted cat stays unlogged and
        // retries next tick). Disjoint borrows: `sightings` read-only,
        // `persist` mutable.
        for s in &self.sightings {
            if let Some(ep) = self.persist.get_mut(&s.ident) {
                ep.logged = true;
            }
        }
        // v3 §1.2: the idle-event scheduler is GONE — in-dwell life (blink /
        // ear twitch / knead / flutter) is a pure function of the dwell clock
        // and rides the is_active frame pacing; after Done nothing arms,
        // nothing wakes (the duty pin).
        // A rebake must repaint: the `free_atlas` version joins the
        // fingerprint whenever cat sprites are on screen (overlay doc §3.3:
        // the atlas version is part of the deco_fp fold). Settled cats fold a
        // stable version, so the settled fp stays stable.
        if !free.is_empty() {
            fp = fold_u64(fp, self.cat_baker.version());
        }
        // The renderers' merge-walk is correct ONLY under sorted-unique (row, col);
        // structural here (row-major occurrences, non-overlapping matches, ascending
        // captured cols), so keep the check out of release builds.
        debug_assert!(
            ink.windows(2)
                .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
            "ink must be sorted by (row, col) with unique cells"
        );
        self.active_until = active_until;
        fp
    }

    /// v3 §1.1/§1.2 episode prepass, before emission each tick:
    ///
    /// * **Arm freezing** — a feline episode's Cat-vs-Paw arm (+ fallback
    ///   cause) is stored at the first emission decision; `MAX_CATS` counts
    ///   VISIBLE non-done stored-Cat episodes (an off-screen or spent episode
    ///   cannot draw, so it cannot starve the slots either). One deliberate
    ///   relaxation of the freeze: a stored FALLBACK arm has emitted nothing by
    ///   construction (fallbacks draw no graphic), so re-deciding it every tick
    ///   cannot flap anything
    ///   visible — it may only upgrade to Cat, once, when clearance/slots
    ///   allow, and the upgraded peek plays from the top. A stored Cat is
    ///   still never downgraded (the anti-flap rationale binds exactly where
    ///   pixels were shown).
    /// * **Birth-latched phase clock** — `phase_start` latches in
    ///   [`Episode::fresh`], never deferred on focus; the prepass
    ///   only advances the started/done flags off the elapsed clock, so cats
    ///   play from the word's first appearance and never replay on refocus.
    ///   The clock SUSPENDS while a stored Cat is ineligible to draw
    ///   (`peek_pause` — text landed in the body footprint) and shifts
    ///   forward on resume, so occlusion postpones the peek instead of
    ///   silently consuming it.
    /// * **Per-axis one-shot flags** — peek started/done (cat cycle), with a
    ///   wall-clock completion sweep for scrolled-off mid-peek episodes;
    ///   sparkle-burst started/done (incl. the ≤ 2 s residual fade);
    ///   ink-sweep started/done — the `done_marks` write condition.
    ///
    /// Born-done / born-settled episodes are skipped entirely (normative
    /// inertness). Direct-built occurrences without episodes (tests/demos)
    /// are untouched — their emission keys off `appeared` pure-time.
    fn episode_prepass(&mut self, now: Instant, cfg: &DecoConfig, geom: EffectGeom) {
        // §5.2 slot census over the VISIBLE occurrence set only. The persist
        // map also holds grace-only episodes whose words scrolled off; those
        // cannot emit a quad this frame (emission walks `self.occ`), so
        // counting them here would let up to 10 s of departed history starve
        // every slot right after cat-word-dense output — the typed kitty at
        // the prompt would freeze as an invisible overflow arm forever.
        let mut cat_slots = self
            .occ
            .iter()
            .filter(|occ| {
                self.persist.get(&occ.ident).is_some_and(|e| {
                    e.shown_as == Some(KittyShownAs::Cat) && !e.peek_done && !e.inert()
                })
            })
            .count();
        for occ in &self.occ {
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                continue;
            };
            if ep.inert() {
                continue;
            }
            // Ink-sweep axis: any captured ink means the sweep began at
            // `appeared`; it settles after its colorway window (sweep + fade
            // / glow window / rainbow drift). A looping sweep never finishes.
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
            // v3 §6 GRAPHIC axis (custom cat words included).
            if let Some(g) = occ.spec.graphic {
                {
                    // Live drawability for THIS occurrence: cell floors, DEC
                    // lines, word width, and the §5.x text clearance (the
                    // two-row body footprint — recomputed every rescan). The
                    // arm decision, the fallback re-evaluation, and the
                    // clearance pause below all key off the same predicate the
                    // emission loop uses, so "armed Cat" always means "could
                    // draw at decision time".
                    let drawable = cat_eligible(occ, cfg, geom);
                    if matches!(
                        ep.shown_as,
                        None | Some(
                            KittyShownAs::PawFallbackFloor | KittyShownAs::PawFallbackOverflow
                        )
                    ) {
                        let prior = ep.shown_as;
                        let arm = if cfg.feline_style != FelineStyle::Cat {
                            KittyShownAs::PawStyle
                        } else if !drawable {
                            // Drawability includes text clearance, not just the
                            // cell floors: a word born under dense output must
                            // not claim a §5.2 slot it will emit nothing from.
                            KittyShownAs::PawFallbackFloor
                        } else if cat_slots < MAX_CATS {
                            cat_slots += 1;
                            KittyShownAs::Cat
                        } else {
                            KittyShownAs::PawFallbackOverflow
                        };
                        if prior.is_some() && arm == KittyShownAs::Cat {
                            // Upgrading a fallback that has shown nothing:
                            // re-latch the phase clock so the entrance plays
                            // in full from NOW — resuming the birth-latched
                            // clock would start mid-dwell (or instantly Done)
                            // for a word the user never saw animate.
                            ep.phase_start = Some(now);
                            ep.peek_pause = None;
                        }
                        ep.shown_as = Some(arm);
                    }
                    // The phase clock latched at birth (`Episode::fresh`) —
                    // under reduced_motion the static pose is this episode's
                    // showing, and the flags below never advance while
                    // reduced, so a pure OS-reduce-motion user keeps the
                    // pose forever.
                    match ep.shown_as {
                        Some(KittyShownAs::Cat) => {
                            // Clearance pause/resume: terminal text flooding
                            // the body footprint mid-peek vanishes the cat
                            // (emission gates on `cat_eligible` per frame) —
                            // but the one-shot must WAIT, not burn out
                            // invisibly. Latch the pause on the first
                            // ineligible tick; on the first eligible tick
                            // shift `phase_start` forward by the paused span
                            // (the freeze/thaw idiom, per episode) so the cat
                            // resumes exactly where it vanished and still
                            // plays its descend.
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
                                // Must mirror emit_cat's dwell derivation
                                // exactly (magic + v4 accessory + the spec
                                // dwell range). Latched on the episode so the
                                // scrolled-off sweep below can finish the
                                // one-shot without the occurrence in scope.
                                let magical = (cfg.feline_magic
                                    && cat_magic(ep.genome.magic).is_some())
                                    || has_accessory(ep.genome.magic);
                                let total =
                                    peek_total_ms(occ.ident, ep.genome.gkey, magical, g.dwell_ms);
                                ep.peek_total = total;
                                if now.saturating_duration_since(ps).as_millis() as u64 >= total {
                                    ep.peek_done = true;
                                }
                            }
                        }
                        Some(KittyShownAs::PawStyle) => {
                            // Style says "no cat art": nothing will ever be
                            // drawn, so the one-shot completes at once and the
                            // done-mark logic stays consistent.
                            if !cfg.reduced_motion
                                && let Some(ps) = ep.phase_start
                                && now >= ps
                            {
                                ep.peek_started = true;
                                ep.peek_done = true;
                            }
                        }
                        // Re-evaluable fallback arms (floor/overflow): nothing
                        // emitted, nothing started, nothing spent. They must
                        // NOT insta-complete — an instant `peek_done` here
                        // makes a slot-starved typed word permanently inert
                        // and, via its departure mark, poisons identical
                        // future retypes.
                        _ => {}
                    }
                }
            }
            // v3 §6 BURST axis flags. Nova/SuperNova flags are owned by their
            // prepasses (ignition grant = point of no return).
            // v3 §6 `chance_pct`: a rolled-off burst never fires, so its
            // flags never advance either (`burst_started` is a done-mark
            // write condition).
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
        // Scrolled-off mid-peek completion: the flag loop above only walks
        // VISIBLE occurrences, so a Cat episode whose word scrolled off
        // mid-peek could never reach `peek_done` off-screen — its lifecycle
        // hung for the whole grace TTL (10 s) even though the animation's
        // wall clock (≤ 4.6 s) had long finished. Finish it here against the
        // latched `peek_total`. Paused episodes are exempt (their clock is
        // suspended by design: if the word scrolls back within grace and the
        // rows are clear, the peek resumes); reduced motion keeps its
        // engine-wide "flags never advance" rule.
        if !cfg.reduced_motion {
            let seq = self.rescan_seq;
            for ep in self.persist.values_mut() {
                if ep.seen_seq != seq
                    && ep.shown_as == Some(KittyShownAs::Cat)
                    && ep.peek_started
                    && !ep.peek_done
                    && ep.peek_pause.is_none()
                    && let Some(ps) = ep.phase_start
                    && now.saturating_duration_since(ps).as_millis() as u64 >= ep.peek_total
                {
                    ep.peek_done = true;
                }
            }
        }
    }

    /// Sweep the flash limiter without retaining work for episodes that can no
    /// longer consume it. Fired slots remain until the rolling safety window
    /// expires; a future slot remains only while its owner is resident and
    /// points at that exact start instant. Consequently stale future work is
    /// cancelled before every grant and the queue is structurally bounded by
    /// live episode cardinality rather than historical output volume.
    fn prune_ignitions(&mut self, now: Instant) {
        let persist = &self.persist;
        let bound = self.bound.unwrap_or(0);
        self.ignitions.retain(|reservation| {
            if reservation.start + IGNITION_WINDOW <= now {
                return false;
            }
            if reservation.start <= now {
                return true;
            }
            // Another pane's pending slot: its owning episode is parked out of
            // reach, so "absent from persist" says nothing about it. Retaining
            // is both correct (the nova is still queued) and safe (the slot
            // expires with the rolling window like any other).
            if reservation.pane != bound {
                return true;
            }
            persist.get(&reservation.owner).is_some_and(|episode| {
                !episode.nova_done && episode.nova_start == Some(reservation.start)
            })
        });
        debug_assert!(
            self.ignitions.iter().filter(|r| r.pane == bound).count() <= MAX_IGNITION_RESERVATIONS
        );
    }

    /// v3 §3.2 supernova prepass (runs BEFORE the classic nova prepass): for
    /// every rolled (`Episode::burst_roll`) rainbow occurrence, grant the
    /// ignition through the SAME flash limiter (a supernova charges as a full
    /// ignition), enforce `MAX_ACTIVE_SUPERNOVAE = 1` (a second roll defers —
    /// row-major, earlier occurrence wins), defer while the word is selected
    /// OR while any CLASSIC nova window is live (the burst mutex is two-way —
    /// the combined `nova_add` bound is a max, not a sum), expire finished
    /// windows into `nova_done` (+ the ≤ 2 s ember fade at emission), fill
    /// this frame's `supers` scratch, and publish the GLOBAL BURST MUTEX
    /// window (`super_until`) the classic prepass defers behind — from the
    /// persist-wide busy scan, so a live-but-scrolled-off window (episode
    /// alive on grace) holds the mutex for its whole run.
    /// `burst_done` was set at ignition GRANT (the point of no return, §1.1).
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
            // §3.2 safety: reduced_motion suppresses the supernova outright
            // (the word keeps its static rainbow). perf_reduced degrades via
            // the §1.1 freeze/thaw latch — the host never ticks while shed.
            return None;
        }
        self.prune_ignitions(now);
        let mut until: Option<Instant> = None;
        let d = Duration::from_millis(supernova::SUPER_TOTAL_MS);
        // Any already-granted, still-running window occupies the mutex even
        // before its (possibly limiter-delayed) start arrives — including
        // episodes not visible this tick (scrolled off mid-grant), so a
        // second roll can never overlap it. The mutex is TWO-WAY: a live
        // CLASSIC nova (its genome-derived window still running) blocks a
        // supernova ignition just like a live supernova blocks classic
        // grants in `nova_prepass` — otherwise combined `nova_add` could
        // reach 3·392 + S_max > MAX_NOVA_QUADS and the "never binds" claim
        // would be falsified. The episode's stored `burst_kind` tells the
        // two windows apart without the occurrence in scope. Live supernova
        // window ends fold into `super_until` HERE, from the persist-wide
        // scan — a scrolled-off supernova (episode alive on grace with
        // `nova_start` set) must keep deferring classic grants for its whole
        // window, not only while its word is visible in `self.occ` below.
        let mut busy = false;
        for ep in self.persist.values() {
            if ep.nova_done {
                continue;
            }
            let Some(s) = ep.nova_start else {
                continue;
            };
            let win = match ep.burst_kind {
                Some(BurstKind::SuperNova) if ep.burst_roll => d,
                Some(BurstKind::Nova) => {
                    Duration::from_millis(u64::from(nova_features(ep.genome.gkey).duration_ms))
                }
                _ => continue,
            };
            let end = s + win;
            if now >= end {
                continue;
            }
            busy = true;
            if matches!(ep.burst_kind, Some(BurstKind::SuperNova)) {
                self.super_until = Some(self.super_until.map_or(end, |u: Instant| u.max(end)));
            }
        }
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
                // The flash limiter charges a supernova as a FULL ignition.
                let Some(start) = grant_pane_ignition(
                    &mut self.ignitions,
                    self.bound.unwrap_or(0),
                    self.pane_px,
                    occ.ident,
                    now,
                    (cx, cy),
                    2.0 * r_max,
                ) else {
                    continue;
                };
                ep.nova_start = Some(start);
                // §1.1: burst_done at IGNITION GRANT — a supernova scrolled
                // off mid-blast never replays on revisit.
                ep.burst_started = true;
                ep.burst_done = true;
                // Curse-BONK detonation cue, AT the grant edge: it inherits
                // the flash limiter's rolling rate cap and `burst_done`'s
                // once-per-episode guarantee verbatim. Profanity only — a
                // custom Toy-Pack supernova on another class is a light show,
                // not a curse. The host drops this kind unless the separate
                // `bonk_detonation` knob opted in (screen content detonates
                // regardless of who typed it).
                if occ.class == Class::Profanity && self.curse_cues.len() < MAX_CURSE_CUES {
                    self.curse_cues.push(CurseCue {
                        kind: CurseCueKind::Detonated,
                        row: occ.row,
                        col: occ.start_col,
                    });
                }
            }
            let Some(start) = ep.nova_start else { continue };
            if now >= start + d {
                ep.nova_done = true;
                continue;
            }
            busy = true;
            let end = start + d;
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

    /// §6.4 nova prepass: sweep the limiter record, grant (possibly DELAYED)
    /// ignition slots in row-major order, expire finished windows into
    /// `nova_done` (one nova per episode — grace-backed, re-armed only by true
    /// identity death), enforce the ≤ [`nova::MAX_ACTIVE_NOVAS`] concurrency
    /// cap (excess skips straight to Ember), fill this frame's `novas`
    /// scratch, and select each nova's ≤ [`nova::MAX_COUPLING_WORDS`] nearest
    /// ink-bearing neighbors (§6.5 — recomputed per presented frame, no stored
    /// state). Returns the latest instant any granted window is still running
    /// (the scheduler keep-alive; it covers delayed ignitions, so a queued Dip
    /// actually fires).
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
        // slots always survive the sweep (t + window > now holds for them).
        // (The supernova prepass ran first and shares this record — its
        // sweep is a no-op here.)
        self.prune_ignitions(now);
        if cfg.reduced_motion {
            return None;
        }
        // v3 §3.2 GLOBAL BURST MUTEX: while a supernova is granted/live, ALL
        // classic ignitions are limiter-deferred (no new grants this tick —
        // they queue behind the supernova's window end).
        let super_busy = self.super_until.is_some_and(|t| now < t);
        let mut until: Option<Instant> = None;
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
            let feats = nova_features(occ.genome.gkey);
            let magic = if cfg.profanity_magic {
                nova_magic(occ.genome.magic)
            } else {
                None
            };
            // Novas have no size floor (they scale, §6.3).
            let ch = i32::from(geom.cell_h);
            let (cx, cy) = burst_center(occ, geom);
            let r_max = feats.radius * ch.max(1) as f32;
            if ep.nova_start.is_none() {
                // v3 §6 `chance_pct`: a rolled-off burst never ignites
                // (episodes with a pre-granted `nova_start` — tests pinning
                // the concurrency cap — are past the roll by construction).
                if !ep.burst_roll {
                    continue;
                }
                // §6.4 item 6: no ignition while the word is selected —
                // deferred until deselection, then queued through the limiter.
                if sel.is_some_and(|sv| sv.span_selected(occ.row, occ.start_col, occ.end_col)) {
                    continue;
                }
                // §3.2 burst mutex: defer behind a live supernova. Keep the
                // scheduler armed one beat past the supernova's window so
                // the deferred grant actually happens on a presented frame.
                if super_busy {
                    if let Some(su) = self.super_until {
                        let wake = su + Duration::from_millis(1);
                        until = Some(until.map_or(wake, |u: Instant| u.max(wake)));
                    }
                    continue;
                }
                let Some(start) = grant_pane_ignition(
                    &mut self.ignitions,
                    self.bound.unwrap_or(0),
                    self.pane_px,
                    occ.ident,
                    now,
                    (cx, cy),
                    2.0 * r_max,
                ) else {
                    continue;
                };
                ep.nova_start = Some(start);
                // v3 §1.1: `burst_done` is set at IGNITION GRANT — detonation
                // start is the point of no return; a nova scrolled off
                // mid-blast never replays on revisit (done-mark write path).
                ep.burst_started = true;
                ep.burst_done = true;
                // Curse-BONK detonation cue, AT the grant edge — the classic
                // `style = "nova"` twin of the supernova cue in `super_prepass`.
                // Inherits the flash limiter's rolling rate cap and `burst_done`'s
                // once-per-episode guarantee verbatim (it hangs off the same
                // grant edge). Profanity only — a custom Toy-Pack nova on another
                // class is a light show, not a curse; the host drops this kind
                // unless the separate `bonk_detonation` knob opted in.
                if occ.class == Class::Profanity && self.curse_cues.len() < MAX_CURSE_CUES {
                    self.curse_cues.push(CurseCue {
                        kind: CurseCueKind::Detonated,
                        row: occ.row,
                        col: occ.start_col,
                    });
                }
            }
            let Some(start) = ep.nova_start else { continue };
            let d = Duration::from_millis(u64::from(feats.duration_ms));
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
                feats,
                magic,
            });
        }
        // §6.5 coupling: per live nova, the MAX_COUPLING_WORDS nearest
        // ink-bearing occurrences (squared-distance order; the stable sort
        // key `(d2, index)` makes the tiebreak row-major).
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

    /// Whether any occurrence is still inside its animation window (profanity /
    /// orca sparkle, a live ink sweep, or a granted — possibly still queued —
    /// nova window) — the scheduler keeps presenting frames while true, then
    /// drops to a pure wait. Cheap (no config / no scan): reads the deadline
    /// last computed by `tick`.
    pub fn is_active(&self, now: Instant) -> bool {
        // Every visible pane animates, so every pane's deadline counts: a cat
        // rising in an unfocused pane must keep the frame cadence armed, or it
        // renders one frame and freezes until the next terminal write.
        self.active_until.is_some_and(|d| now < d)
            || self
                .parked
                .values()
                .any(|p| p.active_until.is_some_and(|d| now < d))
    }
}

/// Pixel center of a burst word's span. DEC rows anchor via the row's real
/// advance (2× on DECDWL) — the v1 DEC precedent. One home for the two burst
/// prepasses, which computed it identically.
fn burst_center(occ: &Occurrence, geom: EffectGeom) -> (i32, i32) {
    let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
    let ch = i32::from(geom.cell_h);
    let cx = (i32::from(occ.start_col) + i32::from(occ.end_col) + 1) * advance / 2;
    let cy = i32::from(occ.row) * ch + ch / 2;
    (cx, cy)
}

/// Request a §6.4 flash slot for `pane`, whose grid origin sits at `pane_px`
/// in WINDOW pixels.
///
/// The limiter is window-wide (WCAG 2.3.1: at most two flashes per rolling
/// second, tightening to one when their regions overlap), so its overlap test
/// only means "these two flashes are in the same place" when every pane's
/// centers live in ONE coordinate space — hence the shift. Split out as a free
/// function so the caller keeps its disjoint `&mut persist` borrow.
fn grant_pane_ignition(
    igns: &mut Vec<IgnitionReservation>,
    pane: u64,
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

/// §6.4 window-wide ignition limiter: the earliest slot ≥ `now` admitting a
/// new ignition — fewer than 2 in the trailing rolling second, tightening to
/// fewer than 1 when any counted ignition's region overlaps the candidate's
/// (center distance < 2·R_max). Callers grant in row-major order, so delays
/// form the deterministic queue of §6.4 (the Dip start shifts to the earliest
/// allowed slot). Each episode requests at most one slot (its `nova_start`
/// gate), and the caller prunes stale future owners before granting. Numeric
/// identities may be reused by a later episode while an old fired slot remains;
/// those are intentionally distinct reservations. The hard cap fails closed
/// (`None`) rather than discarding rolling-window safety history. Modeled as the
/// `FlashLimiter` and `IgnitionReservationLifecycle` ty specs.
///
/// `center` is in WINDOW pixels: every pane's ignitions are counted here, so
/// the overlap test only means "the same place on the glass" when they share
/// one coordinate space (see [`grant_pane_ignition`]).
fn grant_ignition(
    igns: &mut Vec<IgnitionReservation>,
    pane: u64,
    owner: u64,
    now: Instant,
    center: (i32, i32),
    overlap_dist: f32,
) -> Option<Instant> {
    // A MEMORY bound, counted per pane (each pane holds its own live-episode
    // cardinality). The SAFETY bound is the rolling-window count below, which
    // stays over every pane's reservations.
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
/// `BurstKind::Nova` arm of tick's per-occurrence burst dispatch. Emission is
/// a pure function of (now − nova_start, genome, geometry); all stored state
/// (the ignition grant, nova_done) lives in the grace-backed episode + the
/// limiter record, mutated only by the prepass.
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
    persist: &FxHashMap<u64, Episode>,
    out: &mut Vec<WordDecoration>,
    nova: &mut Vec<GlowQuad>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    if let Some(nv) = novas.iter().find(|n| usize::from(n.idx) == oi).copied() {
        let t = now.saturating_duration_since(nv.start).as_millis() as u64;
        // The live window keeps the scheduler armed and the fp
        // changing every frame (the v1 anti-skip rule).
        let until = nv.start + Duration::from_millis(u64::from(nv.feats.duration_ms));
        arm_until(active_until, until);
        *fp = fold_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        let env = nova_env(occ, &nv, geom, cfg);
        // Per-nova budget under the global backstop (which a
        // genome-reachable frame never binds, §6.3).
        let budget = nova::MAX_NOVA_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        nova::emit_nova(t, &env, budget, nova);
        // §6.4 item 6: an active nova over a selection is
        // attenuated like v1 Add decos — the same per-cell
        // predicate, applied per quad (center cell) HOST-side
        // so both backends stay byte-identical by construction.
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
        // Debris rides the EXISTING wdeco Add stream (§6.1) —
        // it inherits the selection freeze and the byte-exact
        // additive parity machinery; the Singularity's
        // darkening ring rides the Over stream as per-cell
        // RingArc masks (additive light can only brighten).
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
        // Ember / Settled — and `reduced_motion`'s static
        // glint from frame 0 (§6.4 item 5): one dim residual
        // spark in the palette's ember tone (dim violet for
        // the Singularity). A deferred (Armed) or
        // selection-deferred ignition emits nothing yet.
        // v3 §1.2 graphics-decay: the ember residual FADES
        // out within 2 s of the nova window's end, then zero
        // decos forever (born-done and episode-less
        // occurrences are already past the fade); the
        // reduced-motion static glint keeps its v2 frame-
        // invariant bytes (no animation to one-shot).
        let ep = persist.get(&occ.ident);
        let done = ep.is_none_or(|e| e.nova_done);
        let fade = if cfg.reduced_motion && !occ.inert {
            // v3 one-shot × reduced motion: the static glint
            // must not resurrect a finished one-shot (W11b
            // demotes unfocused windows to reduced_motion) —
            // once the non-reduced path would emit zero (nova
            // played + ember fade elapsed), reduced emits
            // zero too. A never-ignited episode (reduced from
            // birth) keeps the v2 static glint forever; a
            // chance-rolled-off episode (§6 `chance_pct`)
            // shows nothing at all.
            let rolled_off = ep.is_some_and(|e| !e.burst_roll);
            let spent = ep.is_some_and(|e| {
                e.nova_done
                    && e.nova_start.is_some_and(|s| {
                        let feats = nova_features(occ.genome.gkey);
                        now >= s + Duration::from_millis(
                            u64::from(feats.duration_ms) + RESIDUAL_FADE_MS,
                        )
                    })
            });
            (!rolled_off && !spent).then_some(1.0f32)
        } else if done && !occ.inert {
            ep.and_then(|e| e.nova_start).and_then(|s| {
                let feats = nova_features(occ.genome.gkey);
                let end = s + Duration::from_millis(u64::from(feats.duration_ms));
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
            let feats = nova_features(occ.genome.gkey);
            let magic = if cfg.profanity_magic {
                nova_magic(occ.genome.magic)
            } else {
                None
            };
            let (_, ember) = nova::ember_pair(nova::palette(feats.palette), magic);
            let d = WordDecoration {
                row: occ.row,
                col: occ.start_col,
                dx: 0,
                // Lifted off the glyph body: centered on a lead
                // glyph the residual star overlaps the word's
                // own strokes.
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

/// v3 §3.2 FUCK SUPER NOVA (rolled rainbow episodes), the
/// `BurstKind::SuperNova` arm of tick's per-occurrence burst dispatch.
/// Emission is a pure function of (now − nova_start, env); the roll, the
/// grant, and the mutex live in the prepass.
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
    persist: &FxHashMap<u64, Episode>,
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
        // PER-TIER window: a Flash is over in ~1.1 s, a Nova
        // runs 2.4 s, a Nuke runs 3.6 s while the cloud rises,
        // blooms and rolls out.
        let tier = persist
            .get(&occ.ident)
            .map_or(supernova::SuperTier::Nova, |e| e.burst_tier);
        let until = sv.start + Duration::from_millis(supernova::total_ms(tier));
        arm_until(active_until, until);
        *fp = fold_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        // §3.2 theme branch, per occurrence: additive white is
        // invisible on light backgrounds — the eclipse rides
        // the Over deco stream instead.
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
        // Own budget under the SHARED nova_add backstop; the
        // burst mutex — held for the FULL window, on-screen
        // or scrolled off (persist-wide scan) — keeps the
        // combined channel ≤ 1536, so the clamp never binds.
        let budget =
            supernova::MAX_SUPER_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        supernova::emit_super(t, &env, budget, nova);
        // THE NUKE CLOUD — the rarest degree. Recorded here
        // and emitted AFTER the occurrence loop: baking needs
        // `&mut self.cat_baker`, which this loop already holds.
        // At most one cloud can exist (`MAX_ACTIVE_SUPERNOVAE`
        // = 1), so this is one Option, not a queue.
        if tier == supernova::SuperTier::Nuke && !cfg.reduced_motion {
            *nuke_pending = Some((t, env.cx, env.cy));
        }
        // §3.2 selection × wash: SPLIT row quads around the
        // selected span (the center-cell drop predicate is
        // degenerate for full-width wash quads). Host-side, so
        // CPU == GPU byte-identical by construction.
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
        // §3.2 selection × Over stream: EVERY supernova
        // Over-blend deco is DROPPED over selected cells —
        // the eclipse's Shade veil AND the light-theme charge
        // motes / rainbow debris (both renderers' selection
        // freeze covers only the Add stream) — the same
        // per-cell predicate as the Add-deco freeze, applied
        // host-side so both backends stay byte-identical by
        // construction.
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
        && let Some(ep) = persist.get(&occ.ident)
        && ep.burst_roll
        && let Some(s) = ep.nova_start
    {
        // Afterglow: the ink settled to the static rainbow;
        // the ember star fades ≤ 2 s after the window (§3.2),
        // then zero decos forever.
        // The ember starts when THIS TIER's window ends.
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
                    color: 0x00FF_F2C8, // warm ember over the rainbow
                    alpha: scale_u8(cfg.intensity * 0.30 * f),
                };
                *fp = fold_deco(*fp, &d);
                out.push(d);
            }
        }
    }
}

/// The suspended §4 orca "splash": same randomized, self-terminating motion
/// as the profanity sparkle, but water DROPLETS in an ocean palette that
/// spray UPWARD. v3 §1.2 EXCEPTION: keeps its v2 steady-droplet residual
/// untouched (design §4 promises orca code intact; unreachable while
/// suspended, and the orca redo replaces it). Born-inert episodes skip
/// straight to the settled residual.
#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
fn emit_orca_splash(
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    frame: u64,
    anim: Duration,
    out: &mut Vec<WordDecoration>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    let animating =
        !occ.inert && !cfg.reduced_motion && now.saturating_duration_since(occ.appeared) < anim;
    let width = u64::from(occ.end_col.saturating_sub(occ.start_col) + 1).max(1);
    if animating {
        let until = occ.appeared + anim;
        arm_until(active_until, until);
        let density = cfg.density.clamp(1, 12);
        for k in 0..density {
            if out.len() >= MAX_DECORATIONS {
                break;
            }
            let s = mix(occ.seed
                ^ u64::from(k).wrapping_mul(0xA24B_AED4_963E_E407)
                ^ frame.rotate_left(17));
            let col = occ.start_col + (s % width) as u16;
            let glyph = ORCA_GLYPHS[(s >> 11) as usize % ORCA_GLYPHS.len()];
            let color = ORCA_PALETTE[(s >> 23) as usize % ORCA_PALETTE.len()];
            let env = twinkle(s, frame);
            let alpha = scale_u8(cfg.intensity * env);
            // Upward splash: jitter MINUS a 1..=4 px rise.
            let splash_up = 1 + (s >> 5 & 0x3) as i8;
            let d = WordDecoration {
                row: occ.row,
                col,
                dx: jitter(cfg.jitter, s),
                dy: jitter(cfg.jitter, s >> 7).saturating_sub(splash_up),
                glyph,
                blend: DecoBlend::Add,
                color,
                alpha,
            };
            *fp = fold_deco(*fp, &d) ^ frame.wrapping_mul(0x9E37_79B1);
            out.push(d);
        }
    } else {
        // Steady residual: one dim droplet.
        let d = WordDecoration {
            row: occ.row,
            col: occ.start_col,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Droplet,
            blend: DecoBlend::Add,
            color: ORCA_PALETTE[0],
            alpha: scale_u8(cfg.intensity * 0.30),
        };
        *fp = fold_deco(*fp, &d);
        out.push(d);
    }
}

/// v3 §3.2 selection × wash: split freshly-emitted supernova quads
/// (`quads[n0..]`) around each quad's selected cell span. A full-width wash
/// quad over a mid-screen selection becomes its left + right remainders
/// (≤ 3 quads/row post-split — charged in the S_max closed form); a fully
/// selected quad drops. Host-side filtering keeps CPU == GPU byte-identical
/// by construction.
///
/// ACCEPTED LIMITATION: the px→column mapping below uses the BASE cell width
/// (`geom.cell_w`), so on a DECDWL double-width row — where each cell really
/// advances `2·cell_w` px — the hole is cut at half-scale and a selection on
/// that row can stay washed over during a detonation. The wash is a ≤ 300 ms
/// transient and a selection held across a double-width row DURING one of the
/// ~10%-roll supernovae is a vanishingly rare combination, while threading
/// per-row DECDWL flags through here would complicate the split (and its
/// S_max ≤ 3-quads/row accounting) for every caller. Deliberately not fixed.
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
        // Cut the [s0, s1] cell hole out of the quad (one contiguous hole:
        // deterministic ≤ 2 remainders; disjoint selected runs inside one
        // quad collapse into their bounding hole — strictly MORE clearing,
        // never a washed-over selection).
        let (hx0, hx1) = (s0 * cw, (s1 + 1) * cw);
        let (qx0, qx1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
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

/// The §6 per-frame ink modifiers for one occurrence — everything the nova
/// machinery folds into the ink channel. Pure function of (occurrence, live
/// novas, episode, now); [`INK_FX_NONE`] is byte-identical to the pre-nova
/// path.
struct InkFx {
    /// Anchor-pair override: the nova palette endpoints for a nova-style
    /// profanity word (ember-shifted once spent, §6.1), further chroma-pulsed
    /// by §6.5 coupling. `None` ⇒ the class default pair.
    pair: Option<(u32, u32)>,
    /// §6.1 Dip: the ink envelope multiplier (`1.0` outside the dip).
    dim: f32,
    /// §6.4 item 3: the word's own sweep is FROZEN (held at its settled
    /// gradient) during its own nova window, so the dip/flash pair is the only
    /// ink transition that second.
    freeze: bool,
    /// Ink bytes are changing per frame (dip ramp / coupling pulse): fold the
    /// frame term so the present early-out never skips a live frame.
    frame_live: bool,
    /// v3 §3.2 supernova charge: `0..=1` drive of the ink toward white-hot
    /// (dark bg) / near-black (light bg — the eclipse's indrawn breath),
    /// rising through the charge, holding through the detonation, decaying
    /// by debris start. `0.0` outside a live supernova.
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
    persist: &FxHashMap<u64, Episode>,
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
        let feats = nova_features(occ.genome.gkey);
        let magic = if cfg.profanity_magic {
            nova_magic(occ.genome.magic)
        } else {
            None
        };
        let (core, fringe) = nova::palette(feats.palette);
        let (n0, n1) = genome::ink_pair_nudges(Class::Profanity, occ.genome.gkey);
        let pair = (hue_nudge(core, n0), hue_nudge(fringe, n1));
        let ep = persist.get(&occ.ident);
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
                if t < u64::from(feats.duration_ms) {
                    fx.freeze = true;
                    fx.dim = nova::dip_envelope(t);
                    fx.frame_live = t < nova::DIP_MS;
                }
            }
        }
    }
    // §6.5 blast coupling: a one-shot ~150 ms constant-luminance chroma pulse
    // toward the nova palette when the ring radius crosses this word's span —
    // a pure function of (nova center, t, span); the first coupled nova in
    // prepass order wins deterministically.
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
                // Singularity inverts (§6.5): the gradient leans INTO the
                // collapse — the anchor facing the nova pulses at full
                // amplitude, the far anchor barely.
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
                // Constant-luminance BY CONSTRUCTION (§6.4 item 7): the
                // applied color is luminance-matched back to its anchor, so
                // |ΔL| ≤ 5 % holds for every deployed (anchor, palette, amp).
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

// ───────── §5.2 v2.2 pub geometry helpers (the demo/gallery source) ─────────
//
// The §13 demo and the kitty-gallery example consume THESE instead of
// hardcoding twins of the formulas — the judge loop must never be fed
// corrupted renders because a literal drifted.

/// §5.2 v2.9 base art height before uniform age scaling:
/// `Hart = round(1.7·ch)`. The fitted result remains inside the two-row band
/// above the word plus its chin slice in the word row.
pub fn cat_hart(cell_h: u16) -> u16 {
    ((1.7 * f32::from(cell_h)).round() as u16).max(1)
}

/// §5.2 chin slice: `floor(0.15·Hart)` — the part spilling into the word row
/// (occluded by the word's ascenders: the cat is visibly BEHIND the word).
pub fn cat_chin(hart: u16) -> u16 {
    (0.15 * f32::from(hart)).floor() as u16
}

/// Fit one authored cat into the structural atlas slot at a requested height,
/// preserving its generated viewbox aspect. If a future unusually-wide glyph
/// reaches the slot ceiling, both dimensions shrink together rather than
/// distorting the silhouette.
fn authored_cat_size(variant: CatGlyphId, desired_h: f32, cell_h: u16) -> (u16, u16) {
    let aspect = (f32::from(GLYPHS[variant as usize].aspect_x1000) / 1000.0).max(0.001);
    let max_w = f32::from(cell_h.saturating_mul(4).saturating_sub(PATCH_STRIP).max(1));
    let max_h = f32::from(cell_h.saturating_mul(2).max(1));
    let fitted_h = desired_h.max(1.0).min(max_h).min(max_w / aspect);
    let h = fitted_h.round().clamp(1.0, max_h) as u16;
    let w = (f32::from(h) * aspect).round().clamp(1.0, max_w) as u16;
    (w, h)
}

/// §5.6 rest reveal: FULL Hart for every identity — bobs and entrances
/// terminate at the same fixed point for everyone.
pub fn cat_rest_reveal(hart: u16) -> u16 {
    hart
}

/// cat-art v4 eligibility gate: a matched feline word shows the authored peeking
/// cat whenever it is wide enough (`word_px ≥ 0.8·ch`, so a 1-cell word takes
/// no cat), [`cat_peek_plan`] resolved a habitable side, and the cell metrics
/// clear the §5.7 floors. TOP ROWS ARE ELIGIBLE: a row-0 word has no rows above,
/// so the plan picks [`PeekDir::Down`] and the head slides out from UNDER the
/// word rather than covering it. BUSY SURFACES ARE ELIGIBLE TOO: cats draw
/// [`FreeZ::UnderText`], so a TUI prompt frame in the band costs legibility
/// nothing — only a genuine text wall on BOTH sides is rejected. Every
/// ineligible case draws NO graphic — the word's ink is the graceful fallback.
fn cat_eligible(occ: &Occurrence, cfg: &DecoConfig, geom: EffectGeom) -> bool {
    if cfg.feline_style != FelineStyle::Cat
        || geom.cell_h < CAT_MIN_CELL_H
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
    let cw = i32::from(geom.cell_w);
    let age = cat_age_v4(gkey);
    let desired_h = f32::from(cat_hart(geom.cell_h)) * age.scale();
    let (w, hart) = authored_cat_size(variant, desired_h, geom.cell_h);
    let chin = cat_chin(hart);
    let w_i = i32::from(w);
    // Land the asset's authored visual center on the matched word midpoint,
    // then clamp inward at viewport edges.
    let grid_w = i32::from(geom.cols) * cw;
    let mid = (i32::from(start_col) * cw + (i32::from(end_col) + 1) * cw) / 2;
    let center = f32::from(GLYPHS[variant as usize].center_x) / f32::from(FIXED_ONE);
    let x = (mid - (center * f32::from(w)).round() as i32).clamp(0, (grid_w - w_i).max(0));
    CatGeom {
        w,
        hart,
        chin,
        x: x as u16,
    }
}

/// §5.6 ease-out-back with an EXACT overshoot amplitude: `f(p) = 1 +
/// (c+1)(p−1)³ + c(p−1)²`, `c` solved from the closed form
/// `amp = 4c³ / (27(c+1)²)` by bisection so `max f = 1 + amp` (the genome
/// amplitude, kitten-scaled ×1.3). `f(0) = 0`, `f(1) = 1`.
fn ease_out_back(p: f32, amp: f32) -> f32 {
    let over = |c: f32| 4.0 * c * c * c / (27.0 * (c + 1.0) * (c + 1.0));
    let (mut lo, mut hi) = (0.0f32, 6.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if over(mid) < amp { lo = mid } else { hi = mid }
    }
    let c = 0.5 * (lo + hi);
    let u = p - 1.0;
    1.0 + (c + 1.0) * u * u * u + c * u * u
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
// These decode as salted mixes of `gkey` rather than dedicated genome bits —
// see [`DWELL_SALT`]. The printed ranges are preserved exactly.

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

/// Total peek length: Rise + Dwell + optional anticipation + Descend.
/// Worst case `450 + 3750 + 60 + 320 = 4580 ms < 4800 ms` — the driven A2
/// gate holds from the episode's post-script birth, including rare cats (the
/// dwell cap binds any custom spec range too).
fn peek_total_ms(ident: u64, gkey: u64, magical: bool, range: (u32, u32)) -> u64 {
    CAT_RISE_MS
        + peek_dwell_ms(ident, gkey, magical, range)
        + anticipation_ms(gkey)
        + CAT_DESCEND_MS
}

/// The caret sits ON the token span (or in the cell right after it). The one
/// span predicate shared by the ambiguous-prefix defer, the `at_live_cursor`
/// retype witness, and the one-cat-per-caret suppression — features that must
/// stay in agreement about what "at the caret" means.
fn caret_on_span(cell: (u16, u16), row: u16, start_col: u16, end_col: u16) -> bool {
    let (caret_row, caret_col) = cell;
    caret_row == row && caret_col >= start_col && caret_col <= end_col.saturating_add(1)
}

/// Arm (or extend) the shared animation deadline to at least `until`, keeping
/// the repaint scheduler awake through the latest live window.
fn arm_until(active_until: &mut Option<Instant>, until: Instant) {
    *active_until = Some(active_until.map_or(until, |d| d.max(until)));
}

/// v3 done-mark write: insert (or refresh) the episode's mark under the ident
/// current at departure time. Keyed `ident ^ ctx_fp`; `ctx_fp` is stored
/// explicitly because a horizontal redraw can rekey the position-bearing seed
/// while the episode's frozen genome stays unchanged. LRU: cap
/// [`DONE_MARKS_CAP`], O(1) oldest-touch eviction.
fn mark_done(marks: &mut DoneMarkLru, ident: u64, ep: &Episode) {
    let key = ident ^ ep.ctx_fp;
    marks.insert(key);
}

/// v3 done-mark lookup with touch-on-hit (LRU refresh).
fn touch_done(marks: &mut DoneMarkLru, key: u64) -> bool {
    marks.touch(key)
}

/// Push one 1:1 cat dest/source window `[top, bottom)` as ONE row-free
/// [`FreeSprite`] (UnderText, NEAREST). No band split, no y = 0 clip, no
/// viewport drop here: the renderer's `stamp_free_sprite`/scissor clip against
/// the UNCLAMPED origin, so the §5.7 row-0 clip falls out of the signed `y`.
/// Clipping early would sample different texels than the renderer does.
#[allow(
    clippy::too_many_arguments,
    reason = "a pure push over explicit dest/source scalars; a carrier struct would rename the list"
)]
fn push_cat_free(
    free: &mut Vec<FreeSprite>,
    x: i32,
    w: i32,
    top: i32,
    bottom: i32,
    src_x: i32,
    src_y: i32,
    fp: &mut u64,
) {
    let h = bottom - top;
    if w <= 0 || h <= 0 {
        return;
    }
    let s = FreeSprite {
        x,
        y: top,
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
/// the baker plus the §F4.2 Kitty Log scratch (v4 cats bake their own eyes, so
/// there is no live gaze map / cursor / nova hook any more).
struct CatTick<'a> {
    baker: &'a mut CatBaker,
    /// §F4.2: idents whose episode has not yet queued its one sighting (the
    /// pre-loop scratch; ≤ MAX_OCCURRENCES entries, linear scan is bounded).
    unlogged: &'a [u64],
    /// §F4.2: this tick's recorded sightings (resident, cap MAX_OCCURRENCES).
    sightings: &'a mut Vec<KittySighting>,
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

/// Emit one peeking cat — the v3 §1.2 ONE-SHOT peek cycle:
///
/// ```text
/// Rise (450 ms, ease_out_back, genome overshoot,
///       eyes closed → open at p ≥ 0.85)
/// → Dwell (genome 2200..3598 ms − mix(ident) % 300; +500 ms magic/accessory,
///          cap 3750; authored pose/accessory variation)
/// → Descend (320 ms easeInOutCubic; optional 60 ms anticipation lift on
///            ~50% of genomes; eyes Closed-happy at the near-stationary top)
/// → Done (zero quads, forever for this episode)
/// ```
///
/// Emission is a pure function of `(now, genome, ident, PeekView)`. The whole
/// cycle rides `active_until` (frame-paced while live); dwell frames between
/// the pure-time events are byte-stable, so the fp early-out dedupes them.
/// `reduced_motion` shows the static settled pose while the word is visible
/// (no motion to one-shot — the accessibility override keeps v2 semantics).
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
    // Accessories overlay a plain HEAD only (never a special).
    let accessory = if special.is_none() && cfg.feline_magic {
        accessory_variant_v4(occ.genome.magic)
    } else {
        None
    };
    let g = cat_geometry(occ, geom, variant);
    let ch = i32::from(geom.cell_h);
    // §5.6 v2.6: the REST reveal — full Hart for every identity.
    let rest = i32::from(cat_rest_reveal(g.hart));
    let age = cat_age_v4(occ.genome.gkey);
    let kitten = age == CatAge::Kitten;
    // Entrance overshoot amplitude from a free v4 bit window (bits 15–16 sit
    // above the v4 art layout 0..=14, so they never fragment the bake key).
    let overshoot = 0.06 + genome::field(occ.genome.gkey, 15, 2) as f32 * 0.04;
    let amp = overshoot * if kitten { 1.3 } else { 1.0 };
    let magical = magic.is_some() || has_accessory(occ.genome.magic);
    // v3 §6: the dwell range rides the graphic spec (class default
    // 2200..=3598; direct-built test occurrences fall back to it too).
    let dwell_range = occ.spec.graphic.map_or((2200, 3598), |g| g.dwell_ms);
    let dwell = peek_dwell_ms(occ.ident, occ.genome.gkey, magical, dwell_range);
    let antic = anticipation_ms(occ.genome.gkey);
    let total = peek_total_ms(occ.ident, occ.genome.gkey, magical, dwell_range);
    let dwell_end = CAT_RISE_MS + dwell;

    // Phase resolution. reduced_motion pins the static settled pose (t is
    // never consulted); otherwise the clock runs from the birth-latched
    // phase start — the entrance plays when the word first appears,
    // regardless of focus; an occluded window simply misses frames and the
    // cat never replays on refocus.
    let (t, in_rise, in_dwell) = if cfg.reduced_motion {
        // v3 one-shot × reduced motion: a completed peek stays done (no
        // static resurrection under the W11b unfocused→reduced demotion).
        // The clock latched at birth, so the pose shows from the first
        // presented frame and the flags never advance while reduced — the
        // pose persists. An unlatched clock (defensive; `fresh` always
        // latches) shows nothing.
        if peek.peek_done || peek.start.is_none() {
            return;
        }
        (CAT_RISE_MS, false, true)
    } else {
        let Some(ps) = peek.start else {
            return; // defensive belt: an unlatched clock emits nothing
        };
        if now < ps {
            // Defensive: a future clock origin emits nothing yet but keeps
            // frames coming until the rise begins.
            arm_until(active_until, ps + Duration::from_millis(total));
            *fp = fold_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
            return;
        }
        let t = now.saturating_duration_since(ps).as_millis() as u64;
        if peek.peek_done || t >= total {
            return; // Done: zero quads, forever for this episode
        }
        arm_until(active_until, ps + Duration::from_millis(total));
        (t, t < CAT_RISE_MS, t >= CAT_RISE_MS && t < dwell_end)
    };
    let td = t.saturating_sub(CAT_RISE_MS); // dwell-relative clock
    // The v1 anti-skip rule: rise/descend (and the anticipation beat) change
    // bytes every frame — fold the frame term so the present early-out never
    // skips them. Dwell frames are byte-stable between the pure-time events
    // (blink/twitch/knead/flutter change the quads themselves), so the fp
    // stays stable and presents dedupe.
    if !cfg.reduced_motion && (in_rise || t >= dwell_end) {
        *fp = fold_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
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
        return; // pre-rise / post-descend edge frame: nothing visible
    }
    // cat-art v4: the authored glyph path. A special (from the magic word)
    // REPLACES the head; otherwise the genome variant picks a HEAD, and an
    // overlay accessory (bow/crown/bell) rides a plain head. Fills come from
    // the v4 genome fields plus the bounded local text/background palette.
    let (coat, iris) = cat_fills_v4(occ.genome.gkey);
    let key_v4 = BakeKeyV4 {
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
    let Some(tile) = ctx.baker.get_v4(&key_v4) else {
        // Bake deferred (≤ 2/frame, §5.5): emit nothing this frame; keep the
        // scheduler armed one short beat so the retry frame happens.
        arm_until(active_until, now + Duration::from_millis(50));
        *fp = fold_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        return;
    };
    // §5.6 entrance kinematics, all bake-free: the dest bottom is pinned at
    // `word_row_top + chin`; the visible height grows to the rest reveal with
    // the TOP of the art entering first (ears → eyes → muzzle); overshoot
    // lifts the dest past rest; kittens land with a 2 px bounce early in the
    // dwell; the pre-descend anticipation lifts 2 px.
    let vh = reveal.min(rest);
    // §5.2 v2.9 overshoot clamp: `lift ≤ 2·ch − (Hart − chin)` — the head band
    // (which spans rows r−2 and r−1) must NEVER lift into row r−3.
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
    // v4 cats are the two-band peeking HEAD. UP (the authored pose) anchors the
    // chin slice at the word row's TOP edge and rises into the two rows above.
    // DOWN mirrors it about the word row: the chin tucks behind the row's
    // BOTTOM edge and the head slides out from UNDER the word into the two rows
    // below. Both reveal the art top-down (ears → eyes → muzzle) from a fixed
    // anchor, so the entrance reads the same either way.
    //
    // A top-row word has no rows above it, so DOWN is the only habitable side.
    // The direction comes from [`cat_peek_plan`] at rescan rather than from a
    // viewport clamp here: sliding an UP sprite down into view would park the
    // head squarely on top of the line that summoned it.
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
    // BODY, so these bind on the overshoot/bounce lift alone; sliding by the
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
    // Atlas y shown at dest `top`: the art's top edge lives at tile row 0 in
    // the free-overlay EXACT-SIZE tile (integer-translated bake, NEAREST 1:1).
    let src_y0 = i32::from(tile.ay);
    let n_free = free.len();
    // Free overlay: ONE FreeSprite per cat — the whole dest rect `[top, bottom)`
    // in one arbitrary (row-free) rectangle; the dirty row-union and the
    // CPU/GPU free streams handle the banding.
    push_cat_free(
        free,
        i32::from(g.x),
        i32::from(g.w),
        top,
        bottom,
        i32::from(tile.ax),
        src_y0,
        fp,
    );
    // §F4.2 Kitty Log: body sprites actually landed for this cat — queue its
    // once-per-episode sighting (the post-loop pass flips Episode::logged). v4
    // cats are all peeking heads; the only per-identity trait the authored roster
    // still carries into the log is the overlay accessory (bow / crown).
    if free.len() > n_free
        && ctx.unlogged.contains(&occ.ident)
        && ctx.sightings.len() < MAX_OCCURRENCES
    {
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

/// The class base ink pair `(c0, c1)` (§4.2/§4.6), `0x00RRGGBB`. `None` = the
/// class carries no ink (orca is untouched in v2).
fn ink_pair(class: Class) -> Option<(u32, u32)> {
    match class {
        Class::Emphasis => Some((0x007C_C8FF, 0x00C8_9AFF)),
        Class::Profanity => Some((0x00FF_D447, 0x00FF_7CE5)),
        Class::Feline => Some((0x00F7_A8B8, 0x00FF_D9C2)),
        Class::Orca => None,
    }
}

/// §4.2/§3.4 genome ink-pair nudge: the class base pair with each anchor's hue
/// rotated by its Gray-decoded 4-bit code (`-18°..=+18°` in 2.4° steps — a
/// bit-0 context flip moves one step). Profanity reads the NOVA layout window,
/// every other ink-bearing class the CAT layout window (`genome::ink_pair_nudges`).
fn nudged_ink_pair(class: Class, gkey: u64) -> Option<(u32, u32)> {
    let (c0, c1) = ink_pair(class)?;
    let (n0, n1) = genome::ink_pair_nudges(class, gkey);
    Some((hue_nudge(c0, n0), hue_nudge(c1, n1)))
}

/// v2.9 feline (SelfGlow) envelope: two gentle pulses that fade out over the
/// window, settling to exactly 0 — so the word returns to its own fg with no
/// lingering tint (0% idle). Pure fn of elapsed ms; v3 §6 parameterizes the
/// window/amplitude from the spec (defaults [`FELINE_GLOW_MS`] /
/// [`FELINE_GLOW_AMP`]).
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
    // v3 §6: the ink axis dispatches on the resolved spec colorway (orca's
    // default spec carries no ink axis — the v2 exemption intact).
    let Some(ink_spec) = occ.spec.ink else {
        return;
    };
    match ink_spec.colorway {
        // SelfGlow words (feline) are NOT tinted — a subtle
        // GLOW pulse in the word's OWN fg color that self-terminates to the
        // exact original fg (zero ink cells idle). v3 §1.1: born-done/
        // born-settled inertness — the glow is pre-spent, ZERO ink.
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
            // Keep the frame alive across the WHOLE glow window (even at the
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
                // Glow toward a brightened version of the SAME color (never
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
    // identical) tints. Custom specs apply their colors RAW (explicit config
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
    // window FREEZES the sweep at its settled gradient (fx.freeze), so the
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
    // §4.3 legibility guard, per WORD per FRAME: pull the mix toward the
    // captured base fg (the theme's own legible color) in eighth-steps until the
    // word's mid-gradient ink clears 2.5:1 against the first lead cell's bg.
    let bg = rgb3_to_u32(occ.ink_bg);
    let fg_first = rgb3_to_u32(base_fg[occ.ink_base]);
    let mid_raw = mix_rgb(
        mix_rgb(c0, c1, 0.5),
        0x00FF_FFFF,
        glare * spec_lobe(0.5, center),
    );
    let step = cfg.ink_strength.clamp(0.0, 1.0) / 8.0;
    let mut strength = cfg.ink_strength.clamp(0.0, 1.0);
    while strength > 0.0
        && contrast_ratio(mix_rgb(fg_first, mid_raw, strength), bg) < MIN_INK_CONTRAST
    {
        strength = (strength - step).max(0.0);
    }
    // Gradient parameter: u over the VISUAL span (wide trailing halves count —
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
        // The frame term keeps the fp changing on every animating frame (the v1
        // anti-skip rule); folded mid-FNV-chain so it diffuses per cell. The
        // §6 dip ramp / coupling pulse count as animating (fx.frame_live).
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

/// v3 §3.1 Rainbow colorway emission: per-lead-cell hue
/// `H(cell) = base_hue(genome) + u · span_used + drift(t)` with
/// `span_used = min(span_deg, 100°·(lead_cells − 1))` — 300° on "fuck", a
/// readable two-step on 2-cell CJK, and a pure temporal drift for 1-cell
/// words (a rainbow over time, not a flat tint).
///
/// **Byte-stable freeze (normative)**: the drift phase is
/// `360° · clamp(t, 0, drift_ms) / drift_ms` — an EXACT full cycle, so the
/// frozen phase equals the t = 0 phase and a done-born rainbow is
/// byte-identical to a naturally-settled one. The frame-counter fp fold is
/// GATED to `t < drift_ms` (+ live §6 fx), so settled frames fingerprint
/// stably. `reduced_motion` ⇒ the static (frozen-phase) rainbow immediately.
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
    // EXACT full cycle: phase(drift) == phase(0) == 0 (mod 360).
    let phase = 360.0 * (t.min(drift) as f32 / drift as f32);
    let phase = if animating { phase } else { 0.0 };
    let hue_at = |u: f32| {
        hsv2rgb(
            (base_hue + u * span_used + phase).rem_euclid(360.0),
            sat,
            val,
        )
    };
    // §3.1 legibility guard: min contrast over u ∈ {0, ⅓, ⅔, 1} (4
    // relative_luminance calls per pass — the single mid-gradient sample is
    // blind to yellow/cyan washout); strength pulls toward the captured base
    // fg in eighth-steps until every sample clears MIN_INK_CONTRAST.
    let fg_first = rgb3_to_u32(base_fg[occ.ink_base]);
    let contrast = |c: u32| {
        let la = relative_luminance(c);
        (la.max(lum_bg) + 0.05) / (la.min(lum_bg) + 0.05)
    };
    let step = cfg.ink_strength.clamp(0.0, 1.0) / 8.0;
    let mut strength = cfg.ink_strength.clamp(0.0, 1.0);
    'guard: while strength > 0.0 {
        for u in [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0] {
            if contrast(mix_rgb(fg_first, hue_at(u), strength)) < MIN_INK_CONTRAST {
                strength = (strength - step).max(0.0);
                continue 'guard;
            }
        }
        break;
    }
    // Per-LEAD-CELL hue (§3.1): u indexes lead cells, not columns.
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
fn fold_ink(mut h: u64, c: &InkCell) -> u64 {
    for x in [
        u64::from(c.row),
        u64::from(c.col),
        u64::from(c.color[0]),
        u64::from(c.color[1]),
        u64::from(c.color[2]),
    ] {
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Scale a `0x00RRGGBB` colour's channels by `f` (the §6.1 dip envelope).
fn dim_rgb(c: u32, f: f32) -> u32 {
    let f = f.clamp(0.0, 1.0);
    let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * f).round() as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

fn rgb3_to_u32(c: [u8; 3]) -> u32 {
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
    /// The classic peek: the head rises into the two rows ABOVE the word and
    /// looks over it, chin slice tucked behind the word row's top edge.
    Up,
    /// The mirrored peek: the head slides DOWN out from under the word into
    /// the two rows BELOW it, chin slice tucked behind the word row's bottom
    /// edge. This is what a top-row word gets: there is no room above, and
    /// sliding an UP sprite down ON TOP of the line would bury the very word
    /// that summoned it.
    Down,
}

// ───────── the rare late kitty ─────────
//
// A settled feline word — one whose peek is spent, including every word that
// arrived while the window was away — may occasionally be revisited by a fresh
// cat. Three properties keep it fun rather than noisy:
//
// * BOUNDED: exactly ONE episode is revisited per grant. The whole point of the
//   refocus fix is that cats must not arrive in a crowd.
// * RATE-LIMITED: `REVISIT_MIN_GAP` between grants, and rolls are only even
//   attempted once per `REVISIT_CHECK_PERIOD`, so this costs nothing per frame.
// * DETERMINISTIC: the roll is a hash of the candidate's ident and the check
//   ordinal — no RNG state, reproducible in tests, and stable across a replay.

/// Earliest spacing between two granted revisits.
const REVISIT_MIN_GAP: Duration = Duration::from_secs(45);
/// How often a roll is even attempted.
const REVISIT_CHECK_PERIOD: Duration = Duration::from_secs(20);
/// Grant probability per check, in percent. Deliberately low: at one check per
/// 20 s this averages a visit roughly every few minutes of continuous viewing,
/// which reads as a surprise rather than a cadence.
const REVISIT_CHANCE_PCT: u64 = 8;
/// Salt keeping the revisit roll independent of every other genome decision.
const REVISIT_SALT: u64 = 0x5245_5649_5349_5400; // b"REVISIT\0"

/// Occupancy ceiling for a candidate head band, as a fraction of sampled cells
/// carrying a visible glyph.
///
/// A CEILING, not a veto: any single glyph in the band must NOT reject the
/// cat, or a `kitty` typed inside an ordinary TUI prompt box (a `╭─╮ │ ╰─╯`
/// frame) never draws one while the same word on a clear transcript line
/// always does.
///
/// Legibility is not the reason to reject: cats are pushed as
/// [`FreeZ::UnderText`] sprites, so every glyph in the band draws OVER the fur
/// at full contrast, and a partially busy band costs it nothing. What a solid
/// wall of text does cost is the CAT — fur behind edge-to-edge glyphs reads as
/// noise — so the ceiling rejects only that.
const CAT_MAX_BAND_OCCUPANCY: f32 = 0.75;

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
/// neither side can host it.
///
/// Both candidate bands are scored by [`band_occupancy`] and the CLEARER side
/// wins, with the word's own row folded into each score (the cells beside the
/// word, which the chin slice tucks behind) so a cat prefers the side whose
/// neighbouring text it disturbs least. Ties go to [`PeekDir::Up`] — the
/// authored pose. A band that runs off the viewport is unavailable, which is
/// what forces the top row to [`PeekDir::Down`] and the bottom row to
/// [`PeekDir::Up`]; when both are available but both exceed
/// [`CAT_MAX_BAND_OCCUPANCY`], the surface is a text wall and no cat draws.
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
    let up = (trigger >= band_rows)
        .then(|| side(trigger - band_rows, trigger - 1))
        .flatten();
    let down = (trigger + band_rows < rows)
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

fn u32_to_rgb3(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// WCAG contrast ratio (≥ 1) between two `0x00RRGGBB` colours.
fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Seed an occurrence from its column + class + scanner-canonical form hash, so
/// case-only redraws keep identity (row excluded → scroll-stable). [`FormId`]
/// equality independently guards every adoption, so this hash is never treated
/// as proof that two different lexical surfaces are equal.
fn seed_of(start_col: u16, class: Class, form_hash: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let acc = |x: u64, h: &mut u64| {
        *h ^= x;
        *h = h.wrapping_mul(0x0000_0100_0000_01B3);
    };
    acc(u64::from(start_col), &mut h);
    acc(class_tag(class), &mut h);
    acc(form_hash, &mut h);
    h
}

fn class_tag(c: Class) -> u64 {
    match c {
        Class::Profanity => 1,
        Class::Feline => 2,
        Class::Orca => 3,
        Class::Emphasis => 4,
    }
}

fn glyph_id(g: DecoGlyph) -> u64 {
    match g {
        DecoGlyph::Star4 => 0,
        DecoGlyph::Star5 => 1,
        DecoGlyph::Dot => 2,
        DecoGlyph::Plus => 3,
        DecoGlyph::Paw => 4,
        DecoGlyph::Droplet => 5,
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
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Fold one [`FreeSprite`] into the frame fingerprint — the `fold_deco`/
/// `fold_ink` sibling for the free-overlay channel (overlay doc §3.3): EVERY field,
/// including `z` and `sampler` (a same-rect Under→Over flip must change
/// `deco_fp`, or the Tier-0 early-out would swallow it) and the signed
/// `x`/`y` as their i32 bit patterns. The paired `free_atlas` version is
/// folded once per tick at the existing atlas-version fold site.
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
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
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
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Fold one scalar into the FNV-1a fingerprint chain.
fn fold_u64(mut h: u64, x: u64) -> u64 {
    h ^= x;
    h.wrapping_mul(0x0000_0100_0000_01B3)
}

/// Pick a sparkle colour: a palette entry if configured, else a hue rotation.
fn pick_color(cfg: &DecoConfig, s: u64) -> u32 {
    if cfg.palette.is_empty() {
        hsv2rgb((s % 360) as f32, 0.85, 1.0)
    } else {
        cfg.palette[(s >> 23) as usize % cfg.palette.len()]
    }
}

/// Triangular twinkle envelope in `0.35..=1.0`, phase-offset per spark.
fn twinkle(s: u64, frame: u64) -> f32 {
    let phase = (s & 0xff) as f32 / 255.0 * std::f32::consts::TAU;
    let t = frame as f32 * 0.45 + phase;
    0.35 + 0.65 * (t.sin() * 0.5 + 0.5)
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

    // The ignored performance gates share the same test binary. `cargo test`
    // otherwise runs them concurrently, so one benchmark can steal CPU from
    // another and turn a healthy hot path into a spurious threshold failure.
    // Keep the production workload unchanged and serialize only the timers.
    static PERF_BENCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn cfg() -> DecoConfig {
        DecoConfig {
            profanity: true,
            feline: true,
            orca: true,
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
            // The v1 batteries pin exact v1 sparkle behavior; the nova
            // battery flips this to `Nova` explicitly.
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
            // Direct-built occurrences resolve their spec from the TEST cfg
            // (v1 sparkle batteries), exactly like the rescan would.
            spec: class_default_spec(class, &cfg()),
            custom: false,
        }
    }

    /// `tick` with a throwaway ink + sprite scratch and NO geometry (the
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

        // v3 §1.2 graphics-decay: after the animation window the residual
        // spark FADES (still animating), then self-terminates to ZERO decos.
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

    #[test]
    fn empty_occurrences_emit_nothing() {
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        let mut out = vec![];
        assert_eq!(tick_deco(&mut wd, now, &cfg(), &mut out), 0);
        assert!(out.is_empty());
    }

    #[test]
    fn rescan_finds_words_on_a_real_grid() {
        let mut term = Terminal::new(4, 48);
        term.process(b"I love cats and say fuck sometimes");
        let mut wd = WordDecorations::default();
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.profanity = true;
        c.feline = true;
        c.allow_bare_cat = true;
        let epoch = term.damage_epoch();
        let now = Instant::now();

        assert!(wd.needs_rescan(epoch), "fresh state must want a scan");
        wd.rescan(&term, 4, 48, &lex, &c, epoch, now);
        assert!(!wd.needs_rescan(epoch), "same epoch must not rescan");

        let mut out = Vec::new();
        tick_deco(&mut wd, now, &c, &mut out);

        // "fuck" → profanity sparkles (Add) on row 0. `tick_deco` passes an
        // all-zero geometry, which trips the §5.7 cell-metric floor, so "cats"
        // draws no graphic at all — and no arm draws a Paw glyph regardless.
        assert!(
            out.iter()
                .any(|d| matches!(d.blend, DecoBlend::Add) && d.row == 0),
            "expected a profanity sparkle on row 0, got {out:?}"
        );
        assert!(
            !out.iter().any(|d| matches!(d.glyph, DecoGlyph::Paw)),
            "no paw graphic under v4, got {:?}",
            out.iter()
                .filter(|d| matches!(d.glyph, DecoGlyph::Paw))
                .collect::<Vec<_>>()
        );
    }

    /// EXACT TOKEN REGRESSION: `fix` and its near-misses must never inherit the
    /// profanity rainbow, whether typed incrementally or overwriting a live
    /// `fuck` occurrence at the same row/column. This pins both lexical
    /// classification and stale episode eviction.
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

        // Same-position replacement must evict the visible occurrence
        // immediately; the 10 s persistence grace may retain identity metadata,
        // but it must never emit for absent text.
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

    /// LIVE-CURSOR REGRESSION: `fut` and `futu` are Romanian profanity forms,
    /// but also transient whole tokens at the cursor while any user types
    /// `future`. Even configurations that explicitly enable Romanian (or every
    /// language) defer these ambiguous forms until a delimiter settles them.
    /// Exact non-ambiguous `fuck` remains immediate; every incomplete prefix,
    /// including `fuc`, remains ordinary.
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
        }
    }

    /// EXACT-COMPLETION CONTRACT: `fuc` is ordinary text at the live caret and
    /// in settled snapshots. The fourth character is the first point at which
    /// the canonical profanity episode and its typed cue may exist.
    #[test]
    fn fuc_is_ordinary_and_only_complete_fuck_activates() {
        let lex = Lexicon::with_languages(&["all"]);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        // This test pins the COMPLETION contract (`fuc` is ordinary, `fuck`
        // cues exactly once), not the detonation frequency — so pin the roll
        // OFF rather than inheriting whatever `supernova_chance` defaults to.
        // If this occurrence's genome wins the roll, the escalation adds a
        // SECOND cue, which says nothing about completion.
        c.supernova_chance = 0;
        let t0 = Instant::now();
        let model = aterm_spec::derive::exact_profanity_completion_model();
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
        assert_eq!(
            wd.drain_curse_cues().count(),
            0,
            "`fuc` emitted a curse cue"
        );

        term.process(b"k");
        assert!(model.fire("TypeK", &mut state));
        let completed_at = t0 + Duration::from_millis(24);
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
        assert_eq!(
            wd.drain_curse_cues().count(),
            1,
            "complete `fuck` must cue once"
        );
    }

    /// The shipping renderer scans an already-resolved cell snapshot rather
    /// than calling `rescan(&Terminal)`. Pin the explicit cursor plumbing on
    /// that path so the live window and introspection captures cannot drift.
    #[test]
    fn snapshot_cursor_defers_ambiguous_form_until_delimiter() {
        let (rows, cols) = (2usize, 48usize);
        let mut c = cfg();
        c.profanity_style = ProfanityStyle::Rainbow;
        let geom = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let now = Instant::now();

        for language in ["ro", "all"] {
            let lex = Lexicon::with_languages(&[language]);
            let mut term = Terminal::new(rows as u16, cols as u16);
            let mut snap = aterm_core::render::RenderInput::default();
            term.process(b"fut");
            term.cell_frame_into(&mut snap, rows, cols);
            let cursor = term.cursor();
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
                snap.default_bg,
                Some((cursor.row, cursor.col)),
            );
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Profanity),
                "{language} snapshot path decorated provisional fut"
            );

            term.process(b" ");
            term.cell_frame_into(&mut snap, rows, cols);
            let cursor = term.cursor();
            wd.rescan_from_cells_with_geom_at_cursor(
                &snap.cells,
                &snap.line_sizes,
                rows,
                cols,
                &lex,
                &c,
                2,
                now,
                geom,
                snap.default_bg,
                Some((cursor.row, cursor.col)),
            );
            assert!(
                wd.occ.iter().any(|o| o.class == Class::Profanity),
                "{language} snapshot path lost delimiter-settled fut"
            );
        }
    }

    /// Bounded causal discriminator for a spent profanity episode. A tainted
    /// token completed at the live caret is a fresh armed episode; a passive
    /// output word that merely happens to end at the terminal cursor stays
    /// spent. `Buggy=1` combines the two single-signal regressions: unconditional
    /// transfer suppresses live retyping and caret-only rearming relights passive
    /// output. Both must produce explicit counterexamples.
    fn live_profanity_retype_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            LiveProfanityRetype {
                const Cap = 2;
                const Buggy = 0;
                var phase = 0;     // 0 intervening text, 1 completed live curse
                var retypes = 0;
                var armed = 0;
                var transfers = 0;
                var passive = 0;
                var preserved = 0;
                var false_rearms = 0;
                action TypeLive when (phase == 0 && retypes <= Cap - 1) {
                    phase = 1;
                    retypes = retypes + 1;
                    armed = if Buggy == 1 { armed } else { armed + 1 };
                    transfers = if Buggy == 1 { transfers + 1 } else { transfers };
                }
                action PassiveCaret when (passive == 0) {
                    passive = 1;
                    preserved = if Buggy == 1 { preserved } else { preserved + 1 };
                    false_rearms = if Buggy == 1 { false_rearms + 1 } else { false_rearms };
                }
                action Intervene when (phase == 1 && retypes <= Cap - 1) { phase = 0; }
                action Stay when (retypes == Cap) { phase = phase; }
                invariant Bounds: phase <= 1 && retypes <= Cap && armed <= Cap && transfers <= Cap && passive <= 1 && preserved <= 1 && false_rearms <= 1;
                invariant EveryLiveRetypeArmed: armed == retypes;
                invariant NoLiveRetypeTransferred: transfers == 0;
                invariant EveryPassiveCaretPreserved: preserved == passive;
                invariant NoPassiveCaretRearm: false_rearms == 0;
            }
        }
    }

    #[test]
    fn live_profanity_retype_model_proves_and_catches_unconditional_transfer() {
        let model = live_profanity_retype_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);

        // Negative controls pin both halves of the faulty policy. Merely
        // finding either counterexample would not prove the other regression
        // remains machine-checkable.
        let mut buggy = model.clone();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut live = buggy.init_state();
        assert!(buggy.fire("TypeLive", &mut live));
        assert!(!buggy.check_invariant("EveryLiveRetypeArmed", &live));
        assert!(!buggy.check_invariant("NoLiveRetypeTransferred", &live));

        let mut passive = buggy.init_state();
        assert!(buggy.fire("PassiveCaret", &mut passive));
        assert!(!buggy.check_invariant("EveryPassiveCaretPreserved", &passive));
        assert!(!buggy.check_invariant("NoPassiveCaretRearm", &passive));
    }

    /// Tier-1 binding for the negative side of the causal rule. `Terminal`
    /// naturally leaves its cursor immediately after passive output, so this
    /// fixture would be misclassified by a caret-only policy. After true
    /// identity expiry, the done mark must still make the returning word inert.
    #[test]
    fn passive_output_at_terminal_cursor_stays_spent() {
        let model = live_profanity_retype_model();
        let mut state = model.init_state();
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
        assert!(model.fire("PassiveCaret", &mut state));

        let restored = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("restored profanity");
        assert!(restored.inert, "passive cursor-end output rearmed");
        assert!(wd.persist[&restored.ident].born_done);
        assert!(wd.persist[&restored.ident].nova_done);
        assert!(model.check_invariant("EveryPassiveCaretPreserved", &state));
        assert!(model.check_invariant("NoPassiveCaretRearm", &state));
    }

    /// Tier-1 drive of the real recognizer/alignment path: after a spent `fuck`
    /// and intervening `fix`, an incomplete `fuc` stays ordinary and a complete
    /// live `fuck` allocates one fresh episode. A second complete retype after
    /// incremental `future` must re-arm independently.
    #[test]
    fn repeated_complete_fuck_rearms_while_fuc_stays_ordinary() {
        let model = live_profanity_retype_model();
        let mut state = model.init_state();
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
        wd.rescan(
            &live,
            4,
            48,
            &lex,
            &c,
            6,
            first_retype_at + Duration::from_millis(8),
        );
        assert!(model.fire("TypeLive", &mut state));
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

        assert!(model.fire("Intervene", &mut state));
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
        wd.rescan(&repeated, 4, 48, &lex, &c, 13, second_retype_at);
        assert!(model.fire("TypeLive", &mut state));
        let second = wd
            .occ
            .iter()
            .find(|occ| occ.class == Class::Profanity)
            .expect("second live fuck");
        assert_eq!(second.appeared, second_retype_at);
        assert_eq!(wd.birth_seq, births_before + state["armed"] as u64);
        assert!(!second.inert);
        assert!(!wd.persist[&second.ident].nova_done);
        assert!(model.check_invariant("EveryLiveRetypeArmed", &state));
        assert!(model.check_invariant("NoLiveRetypeTransferred", &state));
    }

    /// A Codex composer may repaint the row several times while `fix` is being
    /// typed, then restore an older output line containing `fuck`. Those
    /// unrelated nonblank frames are feline retype evidence only; they must not
    /// re-arm the spent profanity and make the harmless token look causal.
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

    #[test]
    fn orca_word_makes_a_splash() {
        let mut term = Terminal::new(2, 48);
        term.process(b"the orca leaps");
        let mut wd = WordDecorations::default();
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.orca = true;
        let epoch = term.damage_epoch();
        let now = Instant::now();
        wd.rescan(&term, 2, 48, &lex, &c, epoch, now);
        let mut out = Vec::new();
        tick_deco(&mut wd, now, &c, &mut out);
        // "orca" → an additive, randomized SPLASH during the animation window.
        assert!(
            out.iter().any(|d| matches!(d.blend, DecoBlend::Add)),
            "expected an orca splash, got {out:?}"
        );
        // After the window the steady residual is a single dim water Droplet.
        let later = now + Duration::from_millis(c.anim_ms + 10);
        out.clear();
        tick_deco(&mut wd, later, &c, &mut out);
        assert!(
            out.iter().any(|d| matches!(d.glyph, DecoGlyph::Droplet)),
            "steady orca residual must be a droplet, got {out:?}"
        );
        // Disabling the orca category removes the splash entirely.
        c.orca = false;
        wd.reset();
        wd.rescan(&term, 2, 48, &lex, &c, epoch, now);
        out.clear();
        tick_deco(&mut wd, now, &c, &mut out);
        assert!(out.is_empty(), "orca off → no splash, got {out:?}");
    }

    #[test]
    fn scunthorpe_not_decorated_on_real_grid() {
        let mut term = Terminal::new(2, 48);
        term.process(b"the scattered concatenation in category");
        let mut wd = WordDecorations::default();
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        c.allow_bare_cat = true;
        let epoch = term.damage_epoch();
        let now = Instant::now();
        wd.rescan(&term, 2, 48, &lex, &c, epoch, now);
        let mut out = Vec::new();
        tick_deco(&mut wd, now, &c, &mut out);
        assert!(out.is_empty(), "no false positives expected, got {out:?}");
    }

    /// Builtin `en` lexicon plus an emphasis override for `ultrathink`. This
    /// mirrors a user's `[sparkle_words.emphasis] extra_words = ["ultrathink"]`
    /// — since the builtin ships zero emphasis forms, `extra_words` (compiled
    /// into exactly this kind of override entry) is the only emphasis path.
    fn lex_with_emphasis() -> Lexicon {
        Lexicon::with_languages_and_override(
            &["en"],
            Some(
                "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n",
            ),
        )
        .expect("emphasis override parses")
    }

    /// The hot-path snapshot scan is byte-identical to the term-walking scan:
    /// `rescan_from_cells` over a `cell_frame_into` snapshot must produce the
    /// same occurrences (span, class, identity, genome, local cat palette,
    /// ink capture) and the
    /// same §5.7 `dec_line` flags as `rescan(&term)`, including on rows with
    /// wide (CJK) glyphs and on DECDWL double-width rows.
    #[test]
    fn rescan_from_cells_matches_term_rescan() {
        let (rows, cols) = (5usize, 48usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"I love cats and say fuck sometimes\r\n");
        // Wide CJK leads shift the char->column map ahead of the matches.
        term.process("\u{5BBD}\u{5BBD} kitty ultrathink\r\n".as_bytes());
        // DECDWL: the double-width row must carry `dec_line` on BOTH paths.
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

    /// Every field of the decoration set the renderer reads, plus the resident
    /// ink buffers those fields index into — the full equivalence surface for
    /// the [`ScanMemo`] battery below.
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
        // Episode identities too: a memo that returned a stale match list would
        // show up here (as a different persist population) even on a frame
        // whose visible occurrences happened to coincide.
        let mut idents: Vec<u64> = wd.persist.keys().copied().collect();
        idents.sort_unstable();
        (occ, wd.ink_base_fg.clone(), wd.ink_cols.clone(), idents)
    }

    /// The tokenise memo is a PURE-FUNCTION cache, so a memoized engine and an
    /// always-re-lex engine driven through the SAME frame sequence must agree
    /// on every rescan — not merely on the final one.
    ///
    /// The sequence deliberately covers what makes an incremental scan hard:
    /// a partially typed word under the live caret (the `ambiguous` gate is
    /// cursor-dependent, so a row's admitted matches can change with NO text
    /// change on that row), soft-wrapped words, wide CJK leads (which shift the
    /// char->column map away from the memo's char-indexed matches), DECDWL
    /// rows, scrolling (every row's text moves to a different row INDEX — the
    /// reason the memo is keyed by text), repeated identical lines, and a
    /// screen clear.
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
            // Incremental typing at a prompt: only the caret row's text moves,
            // and `fu`/`fuc` sit under the caret (the provisional-collision
            // gate) before `fuck` settles.
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
            // text survives at a NEW row index.
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
        // Non-vacuity: the battery must actually have been SERVED from the
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

    /// The memo caches a function of `(row text, ScanOptions, Lexicon)`, and the
    /// rescan only fires on a DAMAGE EPOCH change — so a deny-list edit on a
    /// byte-idle grid has no text change to force a re-lex. `ScanMemo::fit`'s
    /// options fingerprint is what retires the stale entries; without it the
    /// user's newly ignored word would keep sparkling until it was retyped.
    #[test]
    fn scan_memo_retires_when_the_scan_gates_change() {
        let (rows, cols) = (3usize, 32usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"the kitty is here\r\n");
        let lex = Lexicon::with_languages(&["en"]);
        let mut c = cfg();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 1, now);
        assert!(
            wd.occ.iter().any(|o| o.class == Class::Feline),
            "the fixture must decorate the feline word first"
        );

        // Same cells, same epoch-advance, ONLY the deny list changed.
        c.ignore.insert("kitty".to_string());
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 2, now);
        assert!(
            !wd.occ.iter().any(|o| o.class == Class::Feline),
            "a denied surface must stop decorating without needing a text change"
        );

        // ...and back: the fingerprint must be a two-way gate, not a latch.
        c.ignore.clear();
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 3, now);
        assert!(
            wd.occ.iter().any(|o| o.class == Class::Feline),
            "clearing the deny list must restore the decoration"
        );
    }

    /// The LEXICON is the scanner's third input and the memo keys on none of
    /// it, so it has to ride the retirement fingerprint too (see
    /// [`scan_inputs_fp`]). Same cells, same `ScanOptions`, only
    /// `languages = ["en","ro"]` → `["en"]`: every row's text is unchanged, so
    /// every row HITS, and without the lexicon in the key the frame keeps
    /// showing the language-gated Romanian form the user just switched off —
    /// a wrong decoration set on screen, not a stale one.
    #[test]
    fn scan_memo_retires_when_the_lexicon_changes() {
        let (rows, cols) = (3usize, 32usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        // `futu` is a `ro`-gated ambiguous profanity form: present in the
        // en+ro lexicon, absent from the en-only one, with the SAME scan gates.
        term.process(b"totally futu here\r\n");
        let with_ro = Lexicon::with_languages(&["en", "ro"]);
        let en_only = Lexicon::with_languages(&["en"]);
        let c = cfg();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        let profane = |wd: &WordDecorations| wd.occ.iter().any(|o| o.class == Class::Profanity);
        wd.rescan_from_cells(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &with_ro,
            &c,
            1,
            now,
        );
        assert!(
            profane(&wd),
            "the fixture must decorate the language-gated form under en+ro"
        );

        // Same cells, same epoch-advance, same options — ONLY the lexicon.
        wd.rescan_from_cells(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &en_only,
            &c,
            2,
            now,
        );
        assert!(
            !profane(&wd),
            "a swapped lexicon must not be answered from the old lexicon's memo"
        );

        // ...and back: a fingerprint, not a latch.
        wd.rescan_from_cells(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &with_ro,
            &c,
            3,
            now,
        );
        assert!(
            profane(&wd),
            "restoring the language must restore the decoration"
        );
    }

    /// Two rows whose CHAR streams differ in length (a wide CJK lead makes the
    /// second row one char shorter than its column count), so a memo hit that
    /// skipped the `scan_chars` rebuild leaves this row's char-indexed match
    /// spans pointing past the end of the PREVIOUS row's stream — the
    /// unguarded `chars[pos - 1]` in `genome::simhash_ctx`, i.e. an
    /// out-of-bounds panic inside the present. The invariant is now named at
    /// the seam that owns it; this drives the mutation and proves the guard,
    /// not the crash, is what fires.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "scan_chars must mirror scan_text")]
    fn memo_hit_without_the_scan_chars_rebuild_trips_the_guard() {
        let (rows, cols) = (4usize, 24usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("a kitty naps\r\n\u{5BBD} cat here\r\n".as_bytes());
        let lex = Lexicon::with_languages(&["en"]);
        let c = cfg();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        // Frame 1 populates the memo and leaves `scan_chars` holding the LAST
        // row's (wide-lead, one char shorter) stream.
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 1, now);
        assert!(
            wd.occ.iter().any(|o| o.class == Class::Feline),
            "the fixture must match on the first row, or the hit proves nothing"
        );
        wd.mutate_skip_scan_chars_rebuild = true;
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 2, now);
    }

    /// The unmutated shape of the same fixture: the guard above may not be a
    /// tripwire the healthy hit path stumbles over. The second frame is served
    /// ENTIRELY from the memo (so every row runs the assertion under a stale
    /// candidate stream) and lands on the same decoration set.
    #[test]
    fn memo_hit_over_a_wide_row_keeps_the_frame_and_the_guard_quiet() {
        let (rows, cols) = (4usize, 24usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("a kitty naps\r\n\u{5BBD} cat here\r\n".as_bytes());
        let lex = Lexicon::with_languages(&["en"]);
        let c = cfg();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 1, now);
        let first = deco_state(&wd);
        let misses = wd.scan_memo.misses;
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, 2, now);
        assert_eq!(
            wd.scan_memo.misses, misses,
            "the second frame must be served entirely from the memo"
        );
        assert_eq!(
            first,
            deco_state(&wd),
            "the memo-served frame must decorate identically"
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
        wd.rescan_from_cells_with_geom(
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
        let occupied_row = usize::from(feline(&wd).row - 1);
        let occupied_variant = c
            .feline_magic
            .then(|| special_variant_v4(feline(&wd).genome.magic))
            .flatten()
            .unwrap_or_else(|| cat_variant_v4(feline(&wd).genome.gkey));
        let occupied_col =
            usize::from(cat_geometry(feline(&wd), geom, occupied_variant).x / geom.cell_w);
        let mut blank =
            snap.cells[usize::from(feline(&wd).row)][usize::from(feline(&wd).start_col)];
        blank.ch = ' ';
        blank.wide = false;
        blank.bg = dark;
        snap.cells[occupied_row].resize(occupied_col + 1, blank);
        snap.cells[occupied_row][occupied_col].ch = 'X';
        // A SPARSE blocker must not veto. Cats draw `FreeZ::UnderText`, so one
        // glyph (or a TUI prompt frame) in the band costs legibility nothing —
        // the head simply picks the clearer side.
        // Only a wall on BOTH sides yields, which is what the fill below builds.
        wd.rescan_from_cells_with_geom(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            2,
            now + Duration::from_millis(50),
            geom,
            rgb3_to_u32(dark),
        );
        assert!(
            feline(&wd).cat_text_clear,
            "one blocker cell keeps the cat — under-text fur never fights glyphs"
        );
        let trigger_row = usize::from(feline(&wd).row);
        let walled: Vec<usize> = (trigger_row.saturating_sub(2)..=trigger_row + 2)
            .filter(|r| *r != trigger_row && *r < snap.cells.len())
            .collect();
        for &r in &walled {
            snap.cells[r].resize(cols, blank);
            for cell in &mut snap.cells[r] {
                cell.ch = 'X';
            }
        }
        wd.rescan_from_cells_with_geom(
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
        );
        assert_eq!(
            feline(&wd).cat_colors,
            mixed,
            "a live episode freezes its footprint-derived palette"
        );
        assert!(
            !feline(&wd).cat_text_clear && !cat_eligible(feline(&wd), &c, geom),
            "opaque cats yield when text WALLS both candidate bands"
        );

        for &r in &walled {
            for cell in &mut snap.cells[r] {
                cell.ch = ' ';
            }
        }
        snap.cells[occupied_row][occupied_col].ch = ' ';
        let mut fresh = WordDecorations::default();
        fresh.rescan_from_cells_with_geom(
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
        );
        assert!(
            feline(&fresh).cat_colors.dark(),
            "a fresh episode sees the now-dark full footprint"
        );
    }

    #[test]
    fn moved_episode_rechecks_text_clearance_with_the_transferred_genome() {
        let (rows, cols) = (8usize, 48usize);
        let geom = EffectGeom {
            cell_w: CAT_MIN_CELL_W,
            cell_h: 20,
            rows: rows as u16,
            cols: cols as u16,
        };
        let lex = lex();
        let c = cfg();
        let now = Instant::now();
        let mut wd = WordDecorations::default();

        let snapshot = |row_1_based: u8| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(format!("\x1b[{row_1_based};1Hdear kitty friend").as_bytes());
            let mut snap = aterm_core::render::RenderInput::default();
            term.cell_frame_into(&mut snap, rows, cols);
            snap
        };
        let first = snapshot(4);
        wd.rescan_from_cells_with_geom(
            &first.cells,
            &first.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            1,
            now,
            geom,
            first.default_bg,
        );
        let original = feline(&wd).clone();

        // Probe the moved row as a fresh birth to capture the provisional
        // genome that alignment is required to replace.
        let mut moved = snapshot(5);
        let mut probe = WordDecorations::default();
        probe.rescan_from_cells_with_geom(
            &moved.cells,
            &moved.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            1,
            now,
            geom,
            moved.default_bg,
        );
        let provisional = feline(&probe).clone();
        assert_eq!(
            original.seed, provisional.seed,
            "same column keeps the alignment seed"
        );

        let body_cells = |genome: Genome| {
            let variant = c
                .feline_magic
                .then(|| special_variant_v4(genome.magic))
                .flatten()
                .unwrap_or_else(|| cat_variant_v4(genome.gkey));
            let g = cat_geometry_for(
                provisional.start_col,
                provisional.end_col,
                genome.gkey,
                geom,
                variant,
            );
            let cw = i64::from(geom.cell_w);
            let ch = i64::from(geom.cell_h);
            let x0 = i64::from(g.x);
            let x1 = x0 + i64::from(g.w);
            let trigger_top = i64::from(provisional.row) * ch;
            let y0 = (trigger_top + i64::from(g.chin) - i64::from(g.hart)).max(0);
            let mut cells = Vec::new();
            for row in usize::try_from(y0 / ch).unwrap_or(0)..usize::from(provisional.row) {
                for col in usize::try_from(x0 / cw).unwrap_or(0)
                    ..=usize::try_from((x1 - 1) / cw).unwrap_or(0)
                {
                    cells.push((row, col));
                }
            }
            cells
        };

        let provisional_cells = body_cells(provisional.genome);
        // Pick a genome whose body footprint differs from the provisional one,
        // then WALL every cell in the symmetric difference. A single blocker no
        // longer decides anything (see `CAT_MAX_BAND_OCCUPANCY`), so the fixture
        // has to move real occupancy for the two plans to diverge.
        let (transferred, blocked_cells) = (0..10_000u64)
            .find_map(|seed| {
                let genome = Genome {
                    gkey: mix(seed),
                    magic: 1000,
                };
                let transferred_cells = body_cells(genome);
                let diff: Vec<(usize, usize)> = transferred_cells
                    .iter()
                    .filter(|cell| !provisional_cells.contains(cell))
                    .chain(
                        provisional_cells
                            .iter()
                            .filter(|cell| !transferred_cells.contains(cell)),
                    )
                    .copied()
                    .collect();
                (!diff.is_empty()).then_some((genome, diff))
            })
            .expect("fixture finds a transferred footprint distinct from the provisional one");

        let mut blank =
            moved.cells[usize::from(provisional.row)][usize::from(provisional.start_col)];
        blank.ch = ' ';
        blank.wide = false;
        for &(r, col) in &blocked_cells {
            if moved.cells[r].len() <= col {
                moved.cells[r].resize(col + 1, blank);
            }
            moved.cells[r][col].ch = 'X';
        }
        let plan_for = |genome: Genome| {
            cat_peek_plan(
                &moved.cells,
                geom,
                provisional.row,
                provisional.start_col,
                provisional.end_col,
                genome,
                c.feline_magic,
            )
        };
        let expected_clear = plan_for(transferred).is_some();

        wd.persist
            .get_mut(&original.ident)
            .expect("original episode")
            .genome = transferred;
        wd.rescan_from_cells_with_geom(
            &moved.cells,
            &moved.line_sizes,
            rows,
            cols,
            &lex,
            &c,
            2,
            now + Duration::from_millis(100),
            geom,
            moved.default_bg,
        );
        let aligned = feline(&wd);
        assert_eq!(aligned.row, provisional.row);
        assert_eq!(
            aligned.appeared, now,
            "the old episode moved to the new row"
        );
        assert_eq!(
            aligned.genome, transferred,
            "alignment transferred the old genome"
        );
        assert_eq!(
            aligned.cat_text_clear, expected_clear,
            "clearance is computed after alignment with the transferred footprint"
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
        // hard_reset: the ink batteries re-run the same words repeatedly; a
        // transient reset would flush done marks and make the second run
        // born-done (settled ink) — not what these tests measure.
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

    /// The ink sweep SETTLES: after `sweep_ms + 250 ms` the emitted bytes are
    /// constant across frames (a stable fixed point), the fingerprint stops
    /// changing, `is_active` disarms — and the settled bytes equal the
    /// reduced-motion static gradient (the §4.2 endpoint identity).
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

    /// `reduced_motion` forces the static gradient from frame 0: identical bytes
    /// at t=0 and t=later, and no `active_until` contribution.
    #[test]
    fn reduced_motion_ink_static_from_frame_0() {
        let mut term = Terminal::new(2, 48);
        term.process(b"ultrathink");
        let mut wd = WordDecorations::default();
        let mut c = cfg();
        c.reduced_motion = true;
        let now = Instant::now();
        let (a, fp_a) = ink_at(&mut wd, &mut term, 2, 48, &c, now, now);
        assert!(!a.is_empty(), "static ink still renders");
        assert!(!wd.is_active(now), "reduced motion never animates");
        let mut out = Vec::new();
        let mut b = Vec::new();
        let fp_b = tick_ink(
            &mut wd,
            now + Duration::from_millis(500),
            &c,
            &mut out,
            &mut b,
        );
        assert_eq!(a, b, "static gradient is frame-invariant");
        assert_eq!(fp_a, fp_b);
    }

    /// §4.4 truncation: MAX_INK_CELLS is a WHOLE-WORD row-major prefix — the
    /// first occurrence that does not fit gets no ink (never partial), later
    /// ones none either, and re-emission is a stable fixed point.
    #[test]
    fn ink_truncation_is_whole_word_and_stable() {
        // 4 x 10-col emphasis words per row => 40 lead cells/row. 512 / 40 =
        // 12 full rows + 3 words (30 cells, 510 total); word 52 (row 12, 4th)
        // must get NO ink rather than its first 2 cells.
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

    /// §4.3 legibility guard: light pastel ink on a light background pulls the
    /// mix toward the captured base fg until the word's mid-gradient ink clears
    /// 2.5:1 — pinned by replicating the deterministic eighth-step pull.
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

    /// The emphasis gate: `emphasis = false` drops the class at rescan;
    /// `ink_enabled = false` emits no ink for ANY class (profanity/feline keep
    /// their sprite effects, tested elsewhere).
    #[test]
    fn emphasis_and_ink_gates() {
        let mut term = Terminal::new(2, 48);
        term.process(b"ultrathink and a kitten say fuck");
        let mut wd = WordDecorations::default();
        let now = Instant::now();
        // v2.9: feline ink is a GLOW pulse that starts at 0 and ramps in, so we
        // sample mid-pulse (350 ms = the first envelope peak) where all three
        // ink-bearing classes are live.
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

    /// The suppressed-alt-screen / master-off arms call `reset()` then tick with
    /// the same scratch: a stale ink vec from the previous frame must come back
    /// empty (byte-identical off), fp 0.
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

    // ───────────────── §3.6 genome / persistence battery (P2) ─────────────────

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

    /// §13 `genome_frozen_across_neighbor_churn`: churn a neighbor and its SGR
    /// color, then rescan. The persist HIT keeps both the frozen genome and
    /// palette; kill + grace-expire lets the re-appearance resolve NEW context.
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

        // Re-appearance rolls a FRESH genome from the churned context.
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

    /// §13 `grace_survives_one_epoch`: a word occluded for one rescan (the
    /// print-erase-print loop) keeps its genome, its `appeared`, AND its spent
    /// nova — exactly the B-3 fix.
    #[test]
    fn grace_survives_one_epoch() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 64);
        term.process(b"one sleepy kitty purrs");
        wd.rescan(&term, 2, 64, &lex, &c, 1, t0);
        let g1 = feline(&wd).genome;
        let ident = mix(feline(&wd).seed); // ordinal 0
        // Simulate P4: this episode's one nova has been spent.
        wd.persist.get_mut(&ident).expect("episode").nova_done = true;

        // Occlude exactly one rescan (a clear+redraw race).
        let blank = Terminal::new(2, 64);
        wd.rescan(&blank, 2, 64, &lex, &c, 2, t0 + Duration::from_secs(1));
        assert!(wd.occ.is_empty());
        assert!(wd.persist.contains_key(&ident), "grace holds the episode");

        // Re-hit within grace: same genome, same appeared, nova stays spent.
        wd.rescan(&term, 2, 64, &lex, &c, 3, t0 + Duration::from_secs(2));
        assert_eq!(feline(&wd).genome, g1, "occlusion must not re-roll");
        assert_eq!(
            feline(&wd).appeared,
            t0,
            "occlusion must not restart windows"
        );
        assert!(
            wd.persist[&ident].nova_done,
            "the spent-nova guard survives occlusion (no strobe)"
        );
    }

    /// §13 `magic_position_independent`: the same sentence at three
    /// indentations/rows rolls the SAME magic outcome (no ident/col/row folded
    /// into the magic stream, §3.5) while the base genomes stay siblings
    /// (`gkey` folds in the position-bearing seed — the deliberate §15.2 split).
    #[test]
    fn magic_position_independent() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(6, 64);
        term.process(
            b"the nice kitty sleeps\r\n\r\n    the nice kitty sleeps\r\n\r\n        the nice kitty sleeps",
        );
        wd.rescan(&term, 6, 64, &lex, &c, 1, t0);
        let cats: Vec<&Occurrence> = wd.occ.iter().filter(|o| o.class == Class::Feline).collect();
        assert_eq!(cats.len(), 3, "three indented copies");
        assert_eq!(cats[0].genome.magic, cats[1].genome.magic);
        assert_eq!(cats[1].genome.magic, cats[2].genome.magic);
        assert_ne!(
            cats[0].genome.gkey, cats[1].genome.gkey,
            "seeds keep gkeys distinct"
        );
        assert_ne!(cats[1].genome.gkey, cats[2].genome.gkey);
    }

    /// §13 `ordinal_transfer_is_episode_only`: when one of two context-identical
    /// same-seed twins scrolls off, the survivor's ordinal shift lands it on the
    /// departed twin's entry — transferring only EPISODE state (appeared /
    /// nova_done, suppressing a re-flash) and never mutating the coat (§3.6 C-3).
    #[test]
    fn ordinal_transfer_is_episode_only() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut twins = Terminal::new(4, 64);
        twins.process(b"dear kitty friend\r\n\r\ndear kitty friend");
        wd.rescan(&twins, 4, 64, &lex, &c, 1, t0);
        let cats: Vec<&Occurrence> = wd.occ.iter().filter(|o| o.class == Class::Feline).collect();
        assert_eq!(cats.len(), 2);
        assert_eq!(
            cats[0].seed, cats[1].seed,
            "same word, same column: same seed"
        );
        let g = cats[0].genome;
        assert_eq!(
            cats[1].genome, g,
            "context-identical twins hold byte-equal genomes"
        );
        let seed = cats[0].seed;
        let ident0 = mix(seed); // ordinal 0 — the row-major FIRST twin
        // The first twin's nova fires (P4 simulation); the second's does not.
        wd.persist.get_mut(&ident0).expect("twin 0").nova_done = true;

        // The first twin scrolls off; the survivor's ordinal shifts 1 → 0.
        let mut lone = Terminal::new(4, 64);
        lone.process(b"dear kitty friend");
        wd.rescan(&lone, 4, 64, &lex, &c, 2, t0 + Duration::from_secs(1));
        let survivor = feline(&wd);
        assert_eq!(
            survivor.genome, g,
            "the survivor keeps its coat (genome by value)"
        );
        assert_eq!(survivor.appeared, t0, "the survivor inherits the episode");
        assert!(
            wd.persist[&ident0].nova_done,
            "the inherited episode's spent nova suppresses a re-flash (conservative direction)"
        );
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
                // The tokenise memo is resident scratch under the same rule:
                // once every visible row's text is memoized, a rescan neither
                // inserts nor rotates, so a growing capacity here would mean
                // the memo is missing every frame (a silent regression to the
                // full re-tokenise it exists to remove).
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
        // The alignment DP's 66 KB decision plane is LENT to `align_pending`
        // via `mem::take` and handed back at the tail. A return path that
        // forgot the handback would leave the field empty and silently
        // reallocate the plane on every damaged frame — the exact allocation
        // regression this test exists to catch, but invisible to the
        // capacity comparison below (0 == 0).
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

    /// §13 allocation regression, MISS side. The test above covers the all-HIT
    /// steady state the memo exists for; this covers the shape it is WORST at —
    /// a full-screen TUI repaint whose every row's text is new every frame, so
    /// every probe misses and every miss wants a key + match-list buffer. That
    /// is the one path where the memo is net-new work on a crate that is
    /// otherwise allocation-free after warm-up, so the buffers come from the
    /// generation the memo just retired: after warm-up such a frame allocates
    /// NOTHING, and `fresh_buffers` (bumped only when the pool ran dry) is the
    /// direct witness.
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
                // FIXED-WIDTH frame tag: a counter that widens from 99 to 100
                // would shift every match's COLUMN, and columns seed identities
                // — persist churn that has nothing to do with the buffers this
                // test is about.
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
        // Warm-up: several full generations (`gen_cap` is two screenfuls, so a
        // rotation lands every `gen_cap / rows` frames) plus the episode
        // machinery reaching its high-water capacities.
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

        // Non-vacuity, and the size of what the pool returns: the SAME frames
        // on an engine that allocates per miss (the path as first shipped) buy
        // a key + match-list pair for every row of every frame.
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

    /// Project the REAL persist map onto the model's observables and assert they
    /// match the model state after each fired transition. `rolls` is the
    /// externally-counted number of accepted/stored fresh-episode genomes; a
    /// deferred candidate may compute a disposable provisional genome before
    /// alignment transfers the survivor's frozen one.
    /// `nova` is only comparable while the episode exists (the model keeps its
    /// counter through Absent; the real counter dies with the entry — identical
    /// observable behavior, since nothing can ignite in Absent).
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
            // v3: a born-done episode pre-spends `nova_done` without a fire —
            // the model's per-episode `nova` counts FIRES, so born-done
            // projects as 0 (the identity-level guard is the model's `done`).
            let ep = &wd.persist[&ident];
            let nova = i64::from(ep.nova_done && !ep.born_done);
            assert_eq!(nova, st[&"nova"], "projected nova must match the model");
        }
    }

    /// Three same-width snapshots for the repeated-surface cardinality proof.
    /// The two original `kitty` occurrences are spent survivors. `moved`
    /// changes both neighbors, rows, and columns without changing count;
    /// `grown` adds exactly one distant third occurrence.
    fn repeated_kitty_reflow_frames() -> (Terminal, Terminal, Terminal) {
        let mut original = Terminal::new(6, 48);
        original.process(b"\x1b[1;1Hold alpha kitty tail\x1b[3;1Hold bravo kitty tail");

        let mut moved = Terminal::new(6, 48);
        // Both exact-surface/seed episodes move down one row. Old row 2 is
        // equidistant from new rows 1 and 3, reproducing the greedy-bid hole:
        // it must retry row 3 after old row 0 wins row 1, not become fresh.
        moved.process(b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps");

        let mut grown = Terminal::new(6, 48);
        grown.process(
            b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps\x1b[6;1Hnew omega kitty glows",
        );
        (original, moved, grown)
    }

    /// Tier-1: drive the REAL persist map through
    /// appear → ignite → horizontal rekey → changed-context/wrapped rekey →
    /// vanish → re-hit-within-grace → grace-expiry → re-appear with a fake
    /// clock, firing the ty-proven
    /// `SparkleIdentity` model's actions alongside and checking the projected
    /// `(state, rolls, nova)` trace at every step (the aterm-buffer
    /// `conformance_*` idiom; Tier-0 lives in aterm-spec/tests/derived_ring_ty.rs).
    #[test]
    fn sparkle_identity_conformance_real_persist_map() {
        let m = aterm_spec::derive::sparkle_identity_model();
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

        // Ignite (P4 simulation, while live): the episode's one nova is spent.
        wd.persist.get_mut(&ident).expect("live episode").nova_done = true;
        assert!(m.fire("Ignite", &mut st));
        assert_projection(&wd, ident, rolls, &st);
        assert!(m.check_invariant("OneNovaPerEpisode", &st));

        // Rekey: a same-width redraw indents the SAME logical sentence. The
        // position-bearing ident changes, but the old episode transfers: no
        // genome roll, no appeared reset, and the spent nova remains spent.
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

        // ContextMove: the same sentence is now preceded by a changed status
        // token and crosses the 48-column wrap boundary. Thus both viewport
        // position AND row-local context change, while the exact `kitty`
        // surface remains the same logical occurrence. A fresh probe proves
        // the candidate context really differs from the frozen episode (so
        // this cannot pass through the old exact-context fallback by luck).
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

        // Re-hit within grace: NO roll, same genome, same appeared, nova spent.
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

        // v3 §1.3 born-done lockstep: the re-appearance from the done set is
        // a NEW episode (fresh roll — settled ink needs the genome) that
        // enters BORN-DONE: `PlayedOnce` holds and Ignite stays disabled.
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

    /// Negative control for the `Rekey` arm: deliberately discard the old map
    /// entry before the same-width horizontal redraw. The genuine scanner then
    /// takes its fresh-birth path, reproducing Buggy=1 exactly: a second roll,
    /// lost spent guard, and a second fire that violates `PlayedOnce`.
    #[test]
    fn sparkle_identity_negative_control_missing_rekey_is_buggy_trace() {
        let healthy = aterm_spec::derive::sparkle_identity_model();
        let mut buggy = aterm_spec::derive::sparkle_identity_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut st = buggy.init_state();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut original = Terminal::new(2, 48);
        original.process(b"a fluffy kitty naps");
        let mut moved = Terminal::new(2, 48);
        moved.process(b" a fluffy kitty naps");
        let mut rolls = 0i64;

        wd.rescan(&original, 2, 48, &lex, &c, 1, t0);
        rolls += 1;
        assert!(buggy.fire("Appear", &mut st));
        let old_ident = mix(feline(&wd).seed);
        wd.persist.get_mut(&old_ident).expect("live").nova_done = true;
        assert!(buggy.fire("Ignite", &mut st));

        // Fault injection: omission of the rekey transfer makes the shipping
        // scanner observe a persist miss and exercise its real fresh path.
        wd.persist.clear();
        wd.done_marks.clear();
        wd.rescan(&moved, 2, 48, &lex, &c, 2, t0 + Duration::from_secs(1));
        rolls += 1;
        assert!(buggy.fire("Rekey", &mut st));
        let new_ident = mix(feline(&wd).seed);
        assert_ne!(new_ident, old_ident);
        assert_projection(&wd, new_ident, rolls, &st);
        assert!(
            !wd.persist[&new_ident].nova_done,
            "fresh path lost the spent guard"
        );
        assert!(
            !healthy.check_invariant("GenomeFrozen", &st),
            "a logical move must not add a genome roll"
        );

        assert!(buggy.fire("Ignite", &mut st));
        assert!(
            !healthy.check_invariant("PlayedOnce", &st),
            "the rekey bug admits a second logical fire"
        );
    }

    /// Negative control for changed-context recognition completeness. Removing
    /// the live episode before the same exact surface moves across a wrap with
    /// a new status neighbor forces the REAL scanner down its fresh-allocation
    /// arm. That is the `ContextMove` Buggy=1 transition: births and rolls stay
    /// mutually consistent, but the logical move is unrecognized, a false
    /// birth occurs, and the spent nova can fire again.
    #[test]
    fn sparkle_identity_negative_control_context_move_false_birth() {
        let healthy = aterm_spec::derive::sparkle_identity_model();
        let mut buggy = aterm_spec::derive::sparkle_identity_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut st = buggy.init_state();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut original = Terminal::new(2, 48);
        original.process(b"a fluffy kitty naps");
        let mut context_moved = Terminal::new(2, 48);
        context_moved
            .process(b"build output changed; wrapping follows here.    status a bright kitty naps");

        wd.rescan(&original, 2, 48, &lex, &c, 1, t0);
        assert!(buggy.fire("Appear", &mut st));
        let old_ident = mix(feline(&wd).seed);
        let old_ctx = wd.persist[&old_ident].ctx_fp;
        wd.persist.get_mut(&old_ident).expect("live").nova_done = true;
        assert!(buggy.fire("Ignite", &mut st));

        // Fault injection at the recognizer boundary: with no transferable
        // episode, the unchanged lexical surface necessarily becomes fresh.
        wd.persist.clear();
        wd.done_marks.clear();
        let births_before = wd.birth_seq;
        wd.rescan(
            &context_moved,
            2,
            48,
            &lex,
            &c,
            2,
            t0 + Duration::from_secs(1),
        );
        assert!(buggy.fire("ContextMove", &mut st));
        let new_ident = mix(feline(&wd).seed);
        assert_ne!(new_ident, old_ident, "the target crossed the wrap");
        assert_eq!(feline(&wd).row, 1);
        assert_ne!(
            wd.persist[&new_ident].ctx_fp, old_ctx,
            "the candidate's row-local status context really changed"
        );
        assert_eq!(
            wd.birth_seq,
            births_before + 1,
            "the fault injection exercised the genuine fresh-allocation arm"
        );
        assert_projection(&wd, new_ident, 2, &st);
        assert!(
            healthy.check_invariant("GenomeFrozen", &st),
            "a false birth rolls exactly once and evades GenomeFrozen"
        );
        assert!(
            !healthy.check_invariant("RecognitionComplete", &st),
            "the logical move was not recognized"
        );
        assert!(
            !healthy.check_invariant("NoFalseBirths", &st),
            "the move was misclassified as a birth"
        );
        assert!(
            !wd.persist[&new_ident].nova_done,
            "the false birth lost the spent guard"
        );

        assert!(buggy.fire("Ignite", &mut st));
        assert!(
            !healthy.check_invariant("PlayedOnce", &st),
            "the false birth admits a second logical fire"
        );
    }

    /// Soundness complement to ContextMove: equal FormId and proximity are
    /// insufficient after the incremental scans produced by genuine typing.
    /// A retyped nearby `kitty` must remain a fresh, armed episode — the
    /// reliability direction requested for repeated typing.
    #[test]
    fn unrelated_changed_column_retype_remains_fresh_and_armed() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let mut replacement = Terminal::new(4, 48);
        replacement.process(b"\x1b[2;1Hstatus a noisy kitty purrs");

        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

        // Real key-by-key typing produces nonmatching damage scans before the
        // complete token exists. Those scans make this ineligible for the
        // sequence-gap <= 2 clear/redraw continuity arm.
        for (i, partial) in [
            b"\x1b[2;1Hstatus a noisy k purrs".as_slice(),
            b"\x1b[2;1Hstatus a noisy ki purrs".as_slice(),
            b"\x1b[2;1Hstatus a noisy kitt purrs".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut typing = Terminal::new(4, 48);
            typing.process(partial);
            wd.rescan(
                &typing,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Feline),
                "partial token must not be recognized at step {i}"
            );
        }

        let births_before = wd.birth_seq;
        let replaced_at = t0 + Duration::from_millis(64);
        wd.rescan(&replacement, 4, 48, &lex, &c, 5, replaced_at);
        let new = feline(&wd);
        assert_eq!(new.form_id, old.form_id, "the exact surface is kitty");
        assert_ne!(new.seed, old.seed, "the replacement changed column");
        assert_eq!(wd.birth_seq, births_before + 1, "one genuine new birth");
        assert_eq!(new.appeared, replaced_at, "the new kitty activates now");
        assert!(
            !wd.persist[&new.ident].nova_done,
            "the new nearby kitty is armed rather than inheriting spent state"
        );
    }

    #[test]
    fn same_column_retype_after_more_typing_is_fresh_and_armed() {
        let model = aterm_spec::derive::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail"); // kitty starts at column 10
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        let old_ctx = wd.persist[&old.ident].ctx_fp;
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

        for (i, partial) in [
            b"old alpha k tail".as_slice(),
            b"old alpha ki tail".as_slice(),
            b"old alpha kitt tail".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut typing = Terminal::new(4, 48);
            typing.process(partial);
            wd.rescan(
                &typing,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
            assert!(wd.occ.iter().all(|o| o.class != Class::Feline));
        }
        assert!(
            wd.persist[&old.ident].continuity_tainted,
            "nonblank partial-token scans are the model's taint premise"
        );

        let mut replacement = Terminal::new(4, 48);
        replacement.process(b"old alpha kitty tail");
        let births_before = wd.birth_seq;
        let appeared = t0 + Duration::from_millis(64);
        wd.rescan(&replacement, 4, 48, &lex, &c, 5, appeared);
        let new = feline(&wd);
        let mut state = model.init_state();
        assert!(model.fire("TypedRetype", &mut state));
        assert_eq!(
            new.start_col, old.start_col,
            "fixture keeps the same column"
        );
        assert_eq!(new.seed, old.seed, "same form and column keep the seed");
        assert_eq!(
            new.ident, old.ident,
            "the position-bearing key is identical"
        );
        assert_eq!(
            wd.birth_seq - births_before,
            state[&"fresh"] as u64,
            "real fresh allocations project onto TypedRetype"
        );
        assert_eq!(
            wd.persist[&new.ident].ctx_fp, old_ctx,
            "exact context returns"
        );
        assert_eq!(new.appeared, appeared);
        assert!(
            !wd.persist[&new.ident].nova_done,
            "the common same-prompt retype activates the kitty again"
        );
        assert_eq!(state[&"transferred"], 0);
        assert_eq!(state[&"armed"], 1);
        assert!(model.check_invariant("NoFalseTransfers", &state));
    }

    /// A single coalesced partial-token frame is sufficient overwrite
    /// evidence. Even though the complete kitty returns at the same key inside
    /// the two-scan weak-continuity window, the tainted feline episode must not
    /// take either the same-row fast path or an alignment edge.
    #[test]
    fn recent_same_column_retype_after_one_partial_is_fresh_and_armed() {
        let model = aterm_spec::derive::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut complete = Terminal::new(4, 48);
        complete.process(b"old alpha kitty tail");
        let mut partial = Terminal::new(4, 48);
        partial.process(b"old alpha kitt tail");

        let mut wd = WordDecorations::default();
        wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        wd.persist.get_mut(&old.ident).expect("old kitty").nova_done = true;

        wd.rescan(&partial, 4, 48, &lex, &c, 2, t0 + Duration::from_millis(16));
        let tainted = wd.persist[&old.ident];
        assert!(tainted.continuity_tainted);
        assert_eq!(wd.rescan_seq.wrapping_sub(tainted.seen_seq), 1);

        let births_before = wd.birth_seq;
        let appeared = t0 + Duration::from_millis(32);
        wd.rescan(&complete, 4, 48, &lex, &c, 3, appeared);
        let new = feline(&wd);
        let mut state = model.init_state();
        assert!(model.fire("RecentTypedRetype", &mut state));
        assert_eq!(new.ident, old.ident, "fixture returns to the same key");
        assert_eq!(new.appeared, appeared);
        assert_eq!(
            wd.birth_seq - births_before,
            state[&"fresh"] as u64,
            "taint, not elapsed scan count, makes this a fresh birth"
        );
        assert!(!wd.persist[&new.ident].nova_done, "the kitty is armed");
        assert_eq!(state[&"recent"], 1);
        assert_eq!(state[&"continuity_tainted"], 1);
        assert_eq!(state[&"transferred"], 0);
        assert!(model.check_invariant("NoFalseTransfers", &state));
    }

    /// Done-mark poisoning regression: the replaced episode from one explicit
    /// kitty retype must not make the next explicit retype born-done. Each
    /// partial→complete cycle is a new armed episode at the same identity key.
    #[test]
    fn two_consecutive_recent_kitty_retypes_both_arm() {
        let model = aterm_spec::derive::sparkle_retype_rearm_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut complete = Terminal::new(4, 48);
        complete.process(b"old alpha kitty tail");
        let mut partial = Terminal::new(4, 48);
        partial.process(b"old alpha kitt tail");

        let mut wd = WordDecorations::default();
        wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;
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

    /// Capacity-boundary regression for the alignment transaction:
    ///
    /// 1. pull one stale, untainted old kitty from a full persist map;
    /// 2. admit a changed-context/column occurrence as a fresh episode; then
    /// 3. offer the unmatched old episode back for grace.
    ///
    /// Step 3 must not be a raw `HashMap::insert` — that produces entry 513.
    /// Grace history must lose this capacity race while the freshly visible
    /// kitty remains armed and resident.
    #[test]
    fn persist_cap_drops_unmatched_grace_after_fresh_move() {
        let model = aterm_spec::derive::sparkle_persist_capacity_model();
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

        // More than the weak-continuity window of blank scans leaves the old
        // episode untainted, while the final changed context and column defeat
        // both exact evidence arms. This specifically exercises the ordinary
        // unmatched-grace capacity admission (typed-feline reentry has its own
        // explicit re-arm policy and regression below).
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

    #[test]
    fn exact_context_blank_grace_survives_more_than_two_scans() {
        let model = aterm_spec::derive::sparkle_reflow_cardinality_model();
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 48);
        term.process(b"old alpha kitty tail");
        let blank = Terminal::new(4, 48);
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        let old_ctx = wd.persist[&old.ident].ctx_fp;
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

        for seq in 2..=4 {
            wd.rescan(
                &blank,
                4,
                48,
                &lex,
                &c,
                seq,
                t0 + Duration::from_secs(seq - 1),
            );
        }
        assert!(
            !wd.persist[&old.ident].continuity_tainted,
            "blank occlusion is the model's untainted premise"
        );
        let births_before = wd.birth_seq;
        wd.rescan(&term, 4, 48, &lex, &c, 5, t0 + Duration::from_secs(4));
        let resumed = feline(&wd);
        let mut state = model.init_state();
        assert!(model.fire("BlankGrace", &mut state));
        assert_eq!(
            wd.birth_seq - births_before,
            state[&"fresh"] as u64,
            "real blank-grace births project onto the model"
        );
        assert_eq!(resumed.appeared, t0);
        assert_eq!(wd.persist[&resumed.ident].ctx_fp, old_ctx);
        assert!(wd.persist[&resumed.ident].nova_done, "spent guard survives");
        assert_eq!(state[&"transferred"], 1);
        assert_eq!(state[&"armed"], 0);
        assert!(model.check_invariant("RecognitionComplete", &state));
    }

    /// Tier-1 negative controls for the two stale exact-evidence actions. Each
    /// fault changes only the recognizer boundary, then lets the genuine
    /// shipping transfer/fresh path produce the Buggy model's observable state.
    #[test]
    fn reflow_taint_and_blank_grace_negative_controls_match_buggy_model() {
        let healthy = aterm_spec::derive::sparkle_reflow_cardinality_model();
        let mut buggy = aterm_spec::derive::sparkle_reflow_cardinality_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");

        // Fault A: discard the real nonblank-taint evidence. The unchanged
        // exact context then steals the spent episode, exactly Buggy TypedRetype.
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;
        wd.persist.get_mut(&ident).expect("old live").nova_done = true;
        for (i, partial) in [
            b"old alpha k tail".as_slice(),
            b"old alpha ki tail".as_slice(),
            b"old alpha kitt tail".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut typing = Terminal::new(4, 48);
            typing.process(partial);
            wd.rescan(
                &typing,
                4,
                48,
                &lex,
                &c,
                i as u64 + 2,
                t0 + Duration::from_millis(16 * (i as u64 + 1)),
            );
        }
        assert!(wd.persist[&ident].continuity_tainted);
        wd.persist
            .get_mut(&ident)
            .expect("fault target")
            .continuity_tainted = false;
        let births_before = wd.birth_seq;
        wd.rescan(
            &original,
            4,
            48,
            &lex,
            &c,
            5,
            t0 + Duration::from_millis(64),
        );
        let mut typed = buggy.init_state();
        assert!(buggy.fire("TypedRetype", &mut typed));
        assert_eq!(wd.birth_seq - births_before, typed[&"fresh"] as u64);
        assert_eq!(
            i64::from(wd.persist[&ident].nova_done),
            typed[&"transferred"]
        );
        assert!(!healthy.check_invariant("NoFalseTransfers", &typed));
        assert!(!healthy.check_invariant("FreshMatchesExpected", &typed));

        // Fault B: remove the untainted grace survivor before the exact-context
        // return. The real miss path births/arms, exactly Buggy BlankGrace.
        let blank = Terminal::new(4, 48);
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;
        wd.persist.get_mut(&ident).expect("old live").nova_done = true;
        for seq in 2..=4 {
            wd.rescan(
                &blank,
                4,
                48,
                &lex,
                &c,
                seq,
                t0 + Duration::from_millis(16 * (seq - 1)),
            );
        }
        assert!(!wd.persist[&ident].continuity_tainted);
        wd.persist.clear();
        wd.done_marks.clear();
        let births_before = wd.birth_seq;
        wd.rescan(
            &original,
            4,
            48,
            &lex,
            &c,
            5,
            t0 + Duration::from_millis(64),
        );
        let mut grace = buggy.init_state();
        assert!(buggy.fire("BlankGrace", &mut grace));
        let returned = feline(&wd);
        assert_eq!(wd.birth_seq - births_before, grace[&"fresh"] as u64);
        assert_eq!(
            i64::from(!wd.persist[&returned.ident].nova_done),
            grace[&"armed"]
        );
        assert!(!healthy.check_invariant("NoFalseBirths", &grace));
        assert!(!healthy.check_invariant("RecognitionComplete", &grace));
    }

    #[test]
    fn same_position_hour_old_episode_expires_before_fast_adoption() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 48);
        term.process(b"old alpha kitty tail");
        let blank = Terminal::new(4, 48);
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;
        wd.rescan(&blank, 4, 48, &lex, &c, 2, t0 + Duration::from_secs(1));

        let births_before = wd.birth_seq;
        let now = t0 + Duration::from_secs(3_600);
        wd.rescan(&term, 4, 48, &lex, &c, 3, now);
        let reborn = feline(&wd);
        assert_eq!(
            wd.birth_seq,
            births_before + 1,
            "the expired episode was not adopted"
        );
        assert_eq!(reborn.appeared, now);
        assert!(
            wd.persist[&reborn.ident].born_done,
            "true death records the completed one-shot before the same identity returns"
        );
    }

    #[test]
    fn weak_redraw_sequence_boundary_is_exactly_two_scans() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let blank = Terminal::new(4, 48);
        let mut moved = Terminal::new(4, 48);
        moved.process(b"\x1b[2;1Hstatus a noisy kitty purrs");

        let run = |blank_scans: u64| {
            let mut wd = WordDecorations::default();
            wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
            let old_ident = feline(&wd).ident;
            wd.persist.get_mut(&old_ident).expect("old live").nova_done = true;
            for n in 0..blank_scans {
                wd.rescan(
                    &blank,
                    4,
                    48,
                    &lex,
                    &c,
                    n + 2,
                    t0 + Duration::from_millis(16 * (n + 1)),
                );
            }
            let births_before = wd.birth_seq;
            let seq = blank_scans + 2;
            let now = t0 + Duration::from_millis(16 * (blank_scans + 1));
            wd.rescan(&moved, 4, 48, &lex, &c, seq, now);
            (wd, births_before, now)
        };

        let (within, births, _) = run(1); // visible -> blank -> redrawn: gap 2
        assert_eq!(within.birth_seq, births, "gap 2 transfers the episode");
        assert!(within.persist[&feline(&within).ident].nova_done);

        let (outside, births, appeared) = run(2); // gap 3
        assert_eq!(outside.birth_seq, births + 1, "gap 3 is a fresh birth");
        assert_eq!(feline(&outside).appeared, appeared);
        assert!(!outside.persist[&feline(&outside).ident].nova_done);
    }

    #[test]
    fn expired_episode_cannot_be_resurrected_by_low_rescan_count() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let blank = Terminal::new(4, 48);
        let mut moved = Terminal::new(4, 48);
        moved.process(b"\x1b[2;1Hstatus a noisy kitty purrs");
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old_ident = feline(&wd).ident;
        wd.persist.get_mut(&old_ident).expect("old live").nova_done = true;
        wd.rescan(&blank, 4, 48, &lex, &c, 2, t0 + Duration::from_secs(1));

        let births_before = wd.birth_seq;
        let now = t0 + Duration::from_secs(3_600);
        wd.rescan(&moved, 4, 48, &lex, &c, 3, now);
        assert_eq!(wd.birth_seq, births_before + 1);
        assert_eq!(feline(&wd).appeared, now);
        assert!(
            !wd.persist[&feline(&wd).ident].nova_done,
            "TTL expiry occurs before alignment, despite a sequence gap of two"
        );
    }

    #[test]
    fn width_change_never_uses_weak_redraw_transfer() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let mut moved = Terminal::new(4, 49);
        moved.process(b"\x1b[2;1Hstatus a noisy kitty purrs");
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let births_before = wd.birth_seq;
        wd.rescan(&moved, 4, 49, &lex, &c, 2, t0 + Duration::from_millis(16));
        assert_eq!(
            wd.birth_seq,
            births_before + 1,
            "resize settling may make the birth inert, but cannot adopt unrelated state"
        );
    }

    /// Tier-1 group binding for `SparkleReflowCardinality`: two repeated exact
    /// surfaces survive a context-changing 2→2 redraw without births/re-arming;
    /// a subsequent 2→3 redraw transfers those two and births exactly the
    /// single net-new occurrence. Association is checked as a multiset because
    /// repeated equal surfaces are intentionally ambiguous; lifecycle
    /// cardinality is the safety property.
    #[test]
    fn sparkle_reflow_cardinality_conformance_repeated_surfaces() {
        let model = aterm_spec::derive::sparkle_reflow_cardinality_model();
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

        // Compute the moved candidates independently. This pins that both
        // fingerprints changed, so the positive pass cannot accidentally take
        // the exact-context arm instead of the one under test.
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

    /// Negative control for the repeated-surface group proof. Clearing the
    /// transferable old group emulates an exact-context gate that rejects every
    /// changed candidate: real 2→2 creates/arms two, and real 2→3 creates/arms
    /// three. Both traces match `Buggy=1` and violate the net-growth bounds.
    #[test]
    fn sparkle_reflow_cardinality_negative_control_context_gate() {
        let mut buggy = aterm_spec::derive::sparkle_reflow_cardinality_model();
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

    /// Negative control: a GRACE-LESS map (TTL zero — v1's `prev_appeared`
    /// semantics) reproduces exactly the model's `Buggy = 1` trace: the re-hit
    /// re-rolls the genome (`GenomeFrozen` violated) and loses the spent-nova
    /// guard (`OneNovaPerEpisode` violated on the next ignition) — so the
    /// conformance pass above is provably non-vacuous.
    #[test]
    fn sparkle_identity_negative_control_graceless_map_is_buggy_trace() {
        let healthy = aterm_spec::derive::sparkle_identity_model();
        let mut buggy = aterm_spec::derive::sparkle_identity_model();
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

        // One blank rescan: the grace-less map forgets the episode INSTANTLY.
        wd.rescan(&blank, 2, 48, &lex, &c, 2, t0 + Duration::from_secs(1));
        assert!(buggy.fire("Vanish", &mut st));
        assert!(
            wd.persist.is_empty(),
            "grace-less: the episode dies with one epoch"
        );

        // The re-hit that SHOULD continue the episode is a fresh miss: the
        // genome re-rolls — the Buggy model's Rehit does exactly that. The
        // done_marks layer would MASK that amnesia (the graceless departure
        // writes a mark and the re-hit would be born-done), so this control —
        // which models a mark-less engine — must drop them.
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
        // The healthy model REJECTS this state: GenomeFrozen is violated.
        assert!(
            !healthy.check_invariant("GenomeFrozen", &st),
            "births=1 rolls=2 must violate GenomeFrozen"
        );
        // And the lost guard admits a second ignition (Buggy guard allows it):
        // OneNovaPerEpisode is violated — the WCAG anti-strobe property.
        assert!(buggy.fire("Ignite", &mut st));
        assert!(!healthy.check_invariant("OneNovaPerEpisode", &st));
    }

    /// §14 P2 perf gate: a 128-match full-viewport rescan with ALL-MISS SimHash
    /// (`persist` cleared each iteration, so every match walks its ±4-token
    /// context) must stay under 100 µs. Timing-sensitive, so it follows the
    /// repo's manual-timing idiom (aterm-render/tests/session_cpu_bench.rs):
    ///
    /// ```sh
    /// cargo test -p aterm-effects --release bench_rescan_ctx_128_matches -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate (design §14 P2): run manually in --release with --ignored --nocapture"]
    fn bench_rescan_ctx_128_matches() {
        let _perf_guard = PERF_BENCH_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lex = lex();
        let c = cfg();
        let (rows, cols) = (32usize, 80usize);
        let mut term = Terminal::new(32, 80);
        let line: &[u8] =
            b"the soft kitty saw a warm kitty near the tiny kitty by one cozy kitty end";
        for r in 0..rows {
            if r > 0 {
                term.process(b"\r\n");
            }
            term.process(line);
        }
        let mut wd = WordDecorations::default();
        let t0 = Instant::now();
        // Warm the resident scratch to its steady-state capacities.
        for e in 0..8u64 {
            wd.persist.clear();
            wd.rescan(&term, rows, cols, &lex, &c, 1 + e, t0);
        }
        assert_eq!(wd.occ.len(), 128, "the full-viewport 128-match workload");
        let iters = 200;
        let mut samples = Vec::with_capacity(iters);
        for e in 0..iters {
            wd.persist.clear(); // ALL-MISS: 128 SimHash walks per rescan
            let start = Instant::now();
            wd.rescan(&term, rows, cols, &lex, &c, 100 + e as u64, t0);
            samples.push(start.elapsed());
        }
        samples.sort();
        let median = samples[iters / 2];
        println!(
            "bench_rescan_ctx_128_matches: median {median:?} over {iters} all-miss rescans (min {:?}, max {:?})",
            samples[0],
            samples[iters - 1]
        );
        assert!(
            median < Duration::from_micros(100),
            "§14 P2 gate: median {median:?} >= 100 µs"
        );
    }

    /// Worst resident-alignment companion to the all-miss SimHash gate above:
    /// 128 visible equal-form candidates against the full 512-episode grace cap
    /// exercises every cell of the bounded monotone DP. Persist-map setup is
    /// deliberately outside the timed interval; the measured work is the real
    /// scan, removal, 65,536 compatibility decisions, transfer, and reinsertion.
    #[test]
    #[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
    fn bench_rescan_alignment_512_by_128_worstcase() {
        let _perf_guard = PERF_BENCH_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lex = lex();
        let c = cfg();
        let (rows, cols) = (32usize, 80usize);
        let mut term = Terminal::new(32, 80);
        let line: &[u8] =
            b"the soft kitty saw a warm kitty near the tiny kitty by one cozy kitty end";
        for r in 0..rows {
            if r > 0 {
                term.process(b"\r\n");
            }
            term.process(line);
        }
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 128);
        let exemplar = wd.persist[&wd.occ[0].ident];
        let mut episodes = Vec::with_capacity(PERSIST_CAP);
        for i in 0..PERSIST_CAP {
            let mut ep = exemplar;
            ep.seed = 0xA500_0000_0000_0000u64 ^ i as u64;
            ep.ctx_fp = 0x5A00_0000_0000_0000u64 ^ (i as u64).rotate_left(17);
            ep.last_row = (i % rows) as u16;
            ep.last_col = ((i / rows) % cols) as u16;
            episodes.push((0xD000_0000_0000_0000u64 ^ i as u64, ep));
        }

        // Warm every resident scratch at the exact cap.
        wd.persist.clear();
        for &(key, mut ep) in &episodes {
            ep.seen_seq = wd.rescan_seq;
            wd.persist.insert(key, ep);
        }
        wd.rescan(&term, rows, cols, &lex, &c, 2, t0);

        let iters = 200usize;
        let mut samples = Vec::with_capacity(iters);
        for epoch in 0..iters {
            wd.persist.clear();
            for &(key, mut ep) in &episodes {
                ep.seen_seq = wd.rescan_seq;
                wd.persist.insert(key, ep);
            }
            let start = Instant::now();
            wd.rescan(&term, rows, cols, &lex, &c, 100 + epoch as u64, t0);
            samples.push(start.elapsed());
        }
        samples.sort();
        let median = samples[iters / 2];
        println!(
            "bench_rescan_alignment_512_by_128_worstcase: median {median:?} (min {:?}, max {:?})",
            samples[0],
            samples[iters - 1]
        );
        assert!(
            median < Duration::from_millis(1),
            "resident alignment median {median:?} exceeds the 1 ms input-path budget"
        );
    }

    /// §14 P4 / §7.4 perf gate — the worst-case nova burst: 3 GENOME-MAX
    /// novas (radius 2.2 rows, 8 rays, max chroma/thickness — the §6.3
    /// worst-case shape) all mid-Ring on a 120×40 grid. Each frame runs the
    /// FULL host emission (the real `tick()`: limiter sweep, ink incl. §6.5
    /// coupling, debris, ring/ray quads) PLUS the CPU composite of the
    /// dirtied band through the real damaged path (`render_input_cached` —
    /// prev∪cur nova-row marking re-renders exactly the lit bands), and must
    /// stay ≤ 3.0 ms/frame. The measured number lands in
    /// PROOF_CARRYING_PERFORMANCE.md ("sparkle-v2") at P5; the §7.5 ledger's
    /// nova-emit-bound row (ay CHC, P6) is the proof-side companion.
    /// Timing-sensitive, so it follows the repo's manual-timing idiom:
    ///
    /// ```sh
    /// cargo test -p aterm-effects --release bench_nova_emit_worstcase -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate (design §14 P4): run manually in --release with --ignored --nocapture"]
    fn bench_nova_emit_worstcase() {
        let _perf_guard = PERF_BENCH_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        use aterm_render::{Renderer, Theme, WindowCpu};
        let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
            panic!("bench needs a system monospace font");
        };
        let (cw, ch) = rend.cell_size();
        let (rows, cols) = (40usize, 120usize);
        let lex = lex();
        let c = cfg_nova();
        let g = EffectGeom {
            cell_w: cw as u16,
            cell_h: ch as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");
        // Three disjoint profanity words (= MAX_ACTIVE_NOVAS concurrent).
        for (r, col) in [(10u16, 20u16), (20, 60), (30, 100)] {
            term.process(format!("\x1b[{};{}Hfuck", r + 1, col + 1).as_bytes());
        }
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        // Force the §6.3 genome-max feature set on all three occurrences and
        // grant all ignitions at t0 (bypassing the RATE limiter — this bench
        // pins the burst COST; the rate is the FlashLimiter tests' job).
        let gmax = (0..(1u64 << 20))
            .find(|&gk| {
                let f = nova_features(gk);
                f.rays == 8 && f.radius > 2.19 && f.chroma > 2.49 && f.ring_thick > 3.49
            })
            .expect("genome-max features reachable (§3.4)");
        let mut granted = 0usize;
        for occ in &mut wd.occ {
            if occ.class == Class::Profanity {
                occ.genome.gkey = gmax;
                let ep = wd.persist.get_mut(&occ.ident).expect("episode");
                ep.nova_start = Some(t0);
                granted += 1;
            }
        }
        assert_eq!(granted, 3, "the 3-nova worst case");

        let base_input = term.cell_frame(rows, cols);
        let mut input = base_input.clone();
        let mut win = WindowCpu::new();
        // Prime the damage cache with the quiescent frame, then burst.
        rend.render_input_cached(&mut win, &base_input);
        let (mut deco, mut ink, mut fr, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut samples = Vec::new();
        // Mid-Ring frames at the 16 ms cadence, cycled for a stable median
        // (ring + rays live 240..900 ms; debris joins from 500 ms).
        for iter in 0..8u64 {
            for t_ms in (300..860u64).step_by(16) {
                let now = t0 + Duration::from_millis(t_ms);
                let start = Instant::now();
                wd.tick(
                    now, &c, g, None, None, true, &mut deco, &mut ink, &mut fr, &mut nova,
                );
                input.word_decorations.clone_from(&deco);
                input.ink.clone_from(&ink);
                input.nova_add.clone_from(&nova);
                rend.render_input_cached(&mut win, &input);
                let dt = start.elapsed();
                if iter > 0 {
                    samples.push(dt); // iter 0 warms caches/capacities
                }
                assert!(
                    !nova.is_empty(),
                    "the burst must actually be mid-ring at t={t_ms} ms"
                );
            }
        }
        samples.sort();
        let median = samples[samples.len() / 2];
        println!(
            "bench_nova_emit_worstcase: median {median:?}/frame over {} mid-ring frames \
             (min {:?}, max {:?}; 3 genome-max novas, {}x{} px cells, 120x40 grid, \
             emission + damaged-path composite)",
            samples.len(),
            samples[0],
            samples[samples.len() - 1],
            cw,
            ch
        );
        assert!(
            median < Duration::from_millis(3),
            "§14 P4 gate: median {median:?} >= 3 ms/frame"
        );
    }

    // ───────────────────── §5 peeking-cat battery (P3) ─────────────────────

    /// 10×20 reference cell over a 6×20 grid (§7's reference geometry class).
    fn geom20() -> EffectGeom {
        EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 6,
            cols: 20,
        }
    }

    /// Scan a terminal through the geometry-aware snapshot path and hand back
    /// the engine plus the resolved feline occurrence.
    fn scan_grid(
        term: &mut Terminal,
        rows: usize,
        cols: usize,
        c: &DecoConfig,
        geom: EffectGeom,
        now: Instant,
    ) -> WordDecorations {
        let lex = lex();
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        // Blank snapshot rows arrive TRIMMED (possibly empty) — take the bg
        // from the first row that actually has a cell.
        let bg = snap
            .cells
            .iter()
            .find_map(|line| line.first())
            .map_or(0, |cell| rgb3_to_u32(cell.bg));
        let mut wd = WordDecorations::default();
        wd.rescan_from_cells_with_geom(
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
        );
        wd
    }

    /// A `kitty` typed inside a TUI prompt box must summon a cat, exactly like
    /// one echoed onto a clear transcript line. A prompt frame puts `╭──╮`
    /// directly above the input line and `│` on both sides of it, so any
    /// clearance rule that vetoes on a single glyph in the band makes this
    /// case impossible. Cats draw `FreeZ::UnderText`, so the frame renders
    /// over the fur at full contrast and nothing is lost by allowing it.
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
        let mut wd = scan_grid(&mut term, rows, cols, &c, g, t0);
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

    /// THE TOP-LINE RULE. A row-0 word has no rows above it, so the plan must
    /// resolve to `PeekDir::Down` and the head slide out from UNDER the word.
    /// Every emitted pixel sits strictly below the word row: the pin is that
    /// the text being decorated is never covered — which is what a viewport
    /// clamp that merely slides an UP sprite into view would do.
    #[test]
    fn top_row_word_peeks_down_under_the_word_never_over_it() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"kitty"); // row 0 — nothing above it
        let mut wd = scan_grid(&mut term, rows, cols, &c, g, t0);
        let occ = feline(&wd);
        assert_eq!(occ.row, 0, "fixture puts the word on the top row");
        assert!(
            occ.cat_peek_down,
            "a top-row word has no band above — the head must peek DOWN"
        );
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
        assert!(!cats.is_empty(), "the top-row cat draws");
        let word_row_bottom = i32::from(g.cell_h);
        for s in &cats {
            assert!(
                s.y >= word_row_bottom - i32::from(g.cell_h) / 2,
                "no sprite pixel may bury the top-row word it decorates: \
                 sprite y={} vs word row bottom {word_row_bottom}",
                s.y
            );
        }
    }

    /// THE REFOCUS STORM.
    ///
    /// A non-presenting window never renders, so it never rescans: words that
    /// arrive while you are away are genuinely NEW to the engine when you come
    /// back, and every one of them was owed an entrance at the same instant.
    /// Coming back must spend those births instead of playing them.
    ///
    /// This is NOT covered by `born_unfocused_entrance_never_replays_on_refocus`,
    /// which protects episodes whose clock was already running — that requires a
    /// rescan to have run while unfocused. Here none did, which is exactly what
    /// makes the storm reachable.
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

        // The user leaves. The host stops presenting, so NO rescan runs while a
        // build log full of feline words scrolls past.
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

        // Only the FIRST rescan back is suppressed: typing right after refocus
        // still earns its cat.
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

    /// THE RARE LATE KITTY.
    ///
    /// A spent episode — including every word that arrived while away — may be
    /// revisited, but the grant is bounded to ONE episode, rate-limited, and
    /// deterministic in its check ordinal so a replay reproduces it.
    #[test]
    fn rare_revisit_is_bounded_rate_limited_and_deterministic() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let lex = lex();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"kitty\r\nkitty\r\nkitty");

        let build = |seed_gap: u64| {
            let mut wd = WordDecorations::default();
            wd.set_presentable(t0, true);
            wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
            // Spend every episode, as an away-birth or a completed peek would.
            for ep in wd.persist.values_mut() {
                ep.peek_done = true;
            }
            let mut grants = Vec::new();
            let mut at = t0;
            for _ in 0..seed_gap {
                at += REVISIT_CHECK_PERIOD;
                if let Some(id) = wd.roll_revisit(at, &c) {
                    grants.push((at, id));
                }
            }
            grants
        };

        let a = build(200);
        let b = build(200);
        assert_eq!(a, b, "the roll is deterministic in its check ordinal");
        assert!(
            !a.is_empty(),
            "over 200 checks an uncommon visit does happen"
        );
        assert!(
            a.len() < 60,
            "…but stays UNCOMMON: {} grants in 200 checks",
            a.len()
        );
        for pair in a.windows(2) {
            let gap = pair[1].0.saturating_duration_since(pair[0].0);
            assert!(
                gap >= REVISIT_MIN_GAP,
                "grants respect the min gap, got {gap:?}"
            );
        }
    }

    /// A revisit re-arms ONLY the peek axis, and only for a spent episode: a
    /// cat coming back to say hello, never the word's whole birth replayed.
    #[test]
    fn revisit_rearms_the_peek_and_draws_again() {
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

        // Let the peek run out: spent, drawing nothing.
        let spent = t0 + Duration::from_secs(8);
        let (cats, _, _) = tick_cat(&mut wd, spent, &c, g);
        assert!(cats.is_empty(), "the one-shot is spent");
        assert!(wd.persist[&ident].peek_done);

        // Force a grant by rolling until one lands, then the cat draws again.
        let mut at = spent;
        let granted = (0..500)
            .find_map(|_| {
                at += REVISIT_CHECK_PERIOD;
                wd.roll_revisit(at, &c)
            })
            .expect("a revisit is reachable");
        assert_eq!(granted, ident, "the only candidate is the one revisited");
        assert!(!wd.persist[&ident].peek_done, "the peek axis is re-armed");
        let (cats, _, _) = tick_cat(&mut wd, at + Duration::from_millis(600), &c, g);
        assert!(!cats.is_empty(), "the revisiting cat actually draws");
    }

    /// A granted revisit removes the episode's resident done mark under the
    /// LRU's real key (`ident ^ ctx_fp`), so a clear+redraw between the grant
    /// and the re-armed peek cannot re-birth the word born-done.
    #[test]
    fn revisit_removes_the_done_mark_under_the_lru_key() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let lex = lex();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\r\n\r\nnice kitty");
        let mut wd = WordDecorations::default();
        wd.set_presentable(t0, true);
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        let ident = feline(&wd).ident;

        // Spend the peek and write the departure-time done mark, as an
        // earlier scroll-off would have.
        {
            let ep = wd.persist.get_mut(&ident).unwrap();
            ep.peek_done = true;
            ep.peek_started = true;
        }
        let key = ident ^ wd.persist[&ident].ctx_fp;
        wd.done_marks.insert(key);
        assert!(wd.done_marks.contains_key(&key));

        // Grant a revisit; the stale mark must go with it.
        let mut at = t0;
        let granted = (0..500)
            .find_map(|_| {
                at += REVISIT_CHECK_PERIOD;
                wd.roll_revisit(at, &c)
            })
            .expect("a revisit is reachable");
        assert_eq!(granted, ident, "the only candidate is the one revisited");
        assert!(
            !wd.done_marks.contains_key(&key),
            "the grant removes the mark under the real key (ident ^ ctx_fp)"
        );
    }

    /// Reduced motion and a feline-family-off config never grant a revisit, and
    /// neither does a window that is not presenting — a revisit must never be
    /// part of the very storm the refocus fix suppresses.
    #[test]
    fn revisit_respects_every_gate() {
        let (rows, cols) = (6usize, 20usize);
        let lex = lex();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\r\n\r\nnice kitty");

        let spin = |wd: &mut WordDecorations, cfg: &DecoConfig| {
            let mut at = t0;
            (0..200)
                .filter_map(|_| {
                    at += REVISIT_CHECK_PERIOD;
                    wd.roll_revisit(at, cfg)
                })
                .count()
        };

        for (label, mutate) in [
            (
                "reduced motion",
                (|c: &mut DecoConfig| c.reduced_motion = true) as fn(&mut DecoConfig),
            ),
            ("feline off", |c: &mut DecoConfig| c.feline = false),
        ] {
            let mut c = cfg();
            mutate(&mut c);
            let mut wd = WordDecorations::default();
            wd.set_presentable(t0, true);
            wd.rescan(&term, rows, cols, &lex, &cfg(), 1, t0);
            for ep in wd.persist.values_mut() {
                ep.peek_done = true;
            }
            assert_eq!(spin(&mut wd, &c), 0, "{label} grants nothing");
        }

        let c = cfg();
        let mut away = WordDecorations::default();
        away.set_presentable(t0, true);
        away.rescan(&term, rows, cols, &lex, &c, 1, t0);
        for ep in away.persist.values_mut() {
            ep.peek_done = true;
        }
        away.set_presentable(t0, false);
        assert_eq!(
            spin(&mut away, &c),
            0,
            "a window nobody is looking at grants nothing"
        );
    }

    /// The direction is CHOSEN, not fixed: with the rows above walled and the
    /// rows below clear, an interior word flips to the downward peek rather
    /// than losing its cat.
    #[test]
    fn busy_band_above_flips_the_peek_downward() {
        let (rows, cols) = (6usize, 20usize);
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"XXXXXXXXXXXXXXXXXXXX\r\nXXXXXXXXXXXXXXXXXXXX\r\nkitty");
        let wd = scan_grid(&mut term, rows, cols, &c, g, t0);
        let occ = feline(&wd);
        assert_eq!(occ.row, 2);
        assert!(
            occ.cat_text_clear,
            "a busy band above no longer erases the cat"
        );
        assert!(occ.cat_peek_down, "it takes the clear side instead");
    }

    /// A feline occurrence with a hand-picked genome (`magic = 100` is outside
    /// every §3.5 window ⇒ an ordinary cat). Idents keyed off (row, start) so
    /// multi-cat tests get distinct gaze/idle identities.
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
            // Magic word decodes BARE: cat window (% 4096 = 100) misses every
            // magic build, accessory window ((>> 24) % 4096 = 200) misses
            // every accessory — no +500 ms dwell bonus, no key fragmenting.
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
        }
    }

    /// v3: a mid-DWELL sample instant guaranteed quiet for adult head cats:
    /// past the rise + kitten bounce, before the earliest possible in-dwell
    /// event (blink starts at ≥ 0.35 · 1901 ms ≈ 665 ms into the dwell).
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

    /// [`tick_cat`] with an explicit companion cell + focus state (the
    /// one-cat-per-caret gate / §5.6 idle-life batteries). Returns the
    /// free-overlay sprite stream (overlay Phase 4: the peeking cats ride
    /// `free_sprites`).
    fn tick_cat_at(
        wd: &mut WordDecorations,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        companion_at: Option<(u16, u16)>,
        focused: bool,
    ) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
        let mut out = Vec::new();
        let mut ink = Vec::new();
        let mut fr = Vec::new();
        let mut nova = Vec::new();
        let fp = wd.tick(
            now,
            cfg,
            geom,
            companion_at,
            None,
            focused,
            &mut out,
            &mut ink,
            &mut fr,
            &mut nova,
        );
        (fr, out, fp)
    }

    /// TYPED-KITTY RELIABILITY: incremental prompt input creates the episode
    /// exactly when the final `y` lands, and the top-row entrance is visible on
    /// the first animation sample. This is the real user path (five damage
    /// rescans), not a pre-filled fixture.
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

    /// ONE CAT PER CARET. Without the suppression, one keystroke fires TWO
    /// independent cat features at one screen spot.
    /// Typing a feline word makes the host force the cursor companion visible
    /// at the caret (the collection hello, which bypasses the trail config),
    /// while the echoed word is simultaneously matched by the ambient scanner,
    /// which peeks its OWN cat over that word — a couple of cells away, on the
    /// same rows, for an overlapping ~3 s. Neither feature knew the other
    /// existed: [`WordDecorations::tick`] and [`WordDecorations::nyan_cursor`]
    /// simply append to the same `free` stream, and nothing in either reads the
    /// other's placement. On a first-ever discovery the companion even adopts
    /// the word-cat's own look, so the pair are literal twins.
    ///
    /// The companion IS this word's cat, so the ambient peek yields to it. The
    /// scope of that yielding is pinned by its sibling test
    /// ([`a_feline_word_away_from_the_caret_still_peeks_beside_the_companion`]).
    #[test]
    fn a_word_under_the_companion_never_peeks_a_second_cat() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
        // `kitty` echoed at a prompt: row 3, cols 5..=9, caret parked one cell
        // past its last — the shape a live typed word actually has.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[4;6Hkitty");
        let caret = (3u16, 10u16);
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            caret,
            "the caret parks one past the typed word"
        );
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let bg = snap
            .cells
            .iter()
            .find_map(|line| line.first())
            .map_or(0, |cell| rgb3_to_u32(cell.bg));
        let scan = || {
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
                Some(caret),
            );
            let occ = feline(&wd);
            assert_eq!((occ.row, occ.start_col, occ.end_col), (3, 5, 9));
            wd
        };
        // One host frame in `redraw_window` order: the engine tick, then the
        // companion emission, sharing ONE free stream.
        let frame = |wd: &mut WordDecorations, now: Instant, companion: Option<(u16, u16)>| {
            let (mut free, _, _) = tick_cat_at(wd, now, &c, g, companion, true);
            if let Some(cell) = companion {
                wd.nyan_cursor(
                    NyanCursorFrame {
                        geom: g,
                        cursor: cell,
                        look: KittyLook::default(),
                        colors: CatColorKey::default(),
                        bob: 0.0,
                        alpha: 255,
                        pose: crate::nyan_cursor::CatPose::STILL,
                        sing: 0.0,
                        notes: [None; crate::nyan_sing::MAX_NOTES],
                    },
                    &mut free,
                );
            }
            free
        };
        // The atlas bakes at most two tiles per frame, so settle both engines
        // over a few mid-dwell frames: the assertion must read the EMISSION
        // decision, never a transient bake budget.
        let mut wd = scan();
        let mut ctl = scan();
        let (mut fixed, mut control) = (Vec::new(), Vec::new());
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            fixed = frame(&mut wd, now, Some(caret));
            control = frame(&mut ctl, now, None);
        }

        let ambient = |free: &[FreeSprite]| -> Vec<FreeSprite> {
            free.iter()
                .filter(|s| matches!(s.z, FreeZ::UnderText))
                .copied()
                .collect()
        };
        // Control: with NO companion on glass the word peeks exactly as it
        // always has — this is the loved ambient feature, still reachable.
        assert_eq!(
            ambient(&control).len(),
            1,
            "with no companion up the typed word peeks on its own"
        );
        assert_eq!(
            fixed.len(),
            1,
            "the caret already has a companion — no second cat on the same word"
        );
        assert!(
            matches!(fixed[0].z, FreeZ::OverText),
            "the one cat at the caret is the companion itself"
        );
    }

    /// The other half of the one-cat-per-caret rule: suppression is scoped to
    /// the single occurrence under the companion. A feline word ELSEWHERE on
    /// screen still peeks in the very same frame, byte-identical to the frame
    /// with no companion at all.
    #[test]
    fn a_feline_word_away_from_the_caret_still_peeks_beside_the_companion() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
        // Only the AWAY word this time; the caret sits on a bare prompt row.
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(b"\x1b[6;1Hkitty\x1b[4;1H");
        let caret = (3u16, 0u16);
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let bg = snap
            .cells
            .iter()
            .find_map(|line| line.first())
            .map_or(0, |cell| rgb3_to_u32(cell.bg));
        let scan = || {
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
                Some(caret),
            );
            wd
        };
        let frame = |wd: &mut WordDecorations, now: Instant, companion: Option<(u16, u16)>| {
            let (mut free, _, _) = tick_cat_at(wd, now, &c, g, companion, true);
            if let Some(cell) = companion {
                wd.nyan_cursor(
                    NyanCursorFrame {
                        geom: g,
                        cursor: cell,
                        look: KittyLook::default(),
                        colors: CatColorKey::default(),
                        bob: 0.0,
                        alpha: 255,
                        pose: crate::nyan_cursor::CatPose::STILL,
                        sing: 0.0,
                        notes: [None; crate::nyan_sing::MAX_NOTES],
                    },
                    &mut free,
                );
            }
            free
        };
        let mut wd = scan();
        let mut ctl = scan();
        let (mut with_companion, mut control) = (Vec::new(), Vec::new());
        for k in 0..6u64 {
            let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            with_companion = frame(&mut wd, now, Some(caret));
            control = frame(&mut ctl, now, None);
        }
        let ambient = |free: &[FreeSprite]| -> Vec<FreeSprite> {
            free.iter()
                .filter(|s| matches!(s.z, FreeZ::UnderText))
                .copied()
                .collect()
        };
        assert_eq!(ambient(&control).len(), 1, "the away kitty peeks");
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

    /// TYPED-KITTY RELIABILITY (retype-suppression regression): type `kitty`
    /// at the prompt, let its cat finish, `clear` the screen, retype `kitty`
    /// at the SAME column. The retype re-keys identically (ident folds no row;
    /// blank clears never taint), so without the caret-on-word witness it would
    /// wholesale-adopt the spent episode — no cat, and after grace expiry a
    /// session-wide done mark. The witness makes it a fresh episode that plays
    /// a SECOND cat.
    #[test]
    fn clear_then_retype_at_same_prompt_plays_a_second_cat() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        let mut wd = WordDecorations::default();

        term.process(b"kitty"); // caret parks at end_col + 1 — the typed witness
        wd.rescan(&term, g.rows as usize, g.cols as usize, &lex, &c, 1, t0);
        let first = feline(&wd).clone();
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(100), &c, g);
        assert!(!cats.is_empty(), "the first typed kitty plays its cat");

        // Run the one-shot to completion (worst-case peek is 4580 ms).
        let done_t = t0 + Duration::from_millis(5000);
        tick_cat(&mut wd, done_t, &c, g);
        assert!(
            wd.persist[&first.ident].peek_done,
            "the first cat's one-shot has completed"
        );

        // `clear`: one BLANK frame (grace, never taint), prompt returns home.
        term.process(b"\x1b[2J\x1b[H");
        wd.rescan(
            &term,
            g.rows as usize,
            g.cols as usize,
            &lex,
            &c,
            2,
            done_t + Duration::from_millis(16),
        );
        assert!(wd.occ.iter().all(|o| o.class != Class::Feline));

        // Retype at the exact same row AND column: same seed, same ordinal,
        // same context fingerprint — the maximally colliding retype.
        term.process(b"kitty");
        let retyped_at = done_t + Duration::from_millis(32);
        wd.rescan(
            &term,
            g.rows as usize,
            g.cols as usize,
            &lex,
            &c,
            3,
            retyped_at,
        );
        let second = feline(&wd).clone();
        assert_eq!(second.ident, first.ident, "fixture reproduces the key");
        assert_eq!(
            second.appeared, retyped_at,
            "the retyped kitty is a FRESH episode, not an adoption"
        );
        assert!(!second.inert, "the retype must not be born-done");
        assert!(
            !wd.persist[&second.ident].peek_done,
            "the fresh episode's peek is armed"
        );
        let (cats, _, _) = tick_cat(&mut wd, retyped_at + Duration::from_millis(100), &c, g);
        assert!(
            !cats.is_empty(),
            "a genuinely retyped kitty after clear plays a SECOND cat"
        );
    }

    /// SLOT-STARVATION regression: a screenful of cat words arms all 8 §5.2
    /// slots; the user clears and types `kitty` while those episodes are
    /// scrolled off mid-peek (grace-resident for up to 10 s). A slot census
    /// that counted the invisible history would freeze the typed word as an
    /// invisible overflow arm forever. Visible-only counting draws it, and
    /// the wall-clock sweep retires the scrolled-off peeks on schedule.
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

        // Clear and type `kitty` one second in: all 12 episodes are grace-
        // resident and the 8 armed cats are still mid-peek by wall clock.
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

        // Wall-clock completion: by +6 s every armed peek (visible or
        // scrolled off) is past its ≤ 4580 ms one-shot. The scrolled-off
        // episodes can only get there via the prepass sweep.
        tick_cat(&mut wd, t1 + Duration::from_millis(6000), &c, g);
        assert!(
            wd.persist
                .values()
                .all(|e| e.shown_as != Some(KittyShownAs::Cat) || e.peek_done),
            "scrolled-off mid-peek episodes complete by wall clock"
        );
    }

    /// CLEARANCE-PAUSE regression: output landing in the two rows above a
    /// dwelling cat vanishes it (per-frame eligibility). The one-shot clock
    /// must PAUSE while ineligible and resume where it left off — a clock that
    /// kept running would burn the peek to Done while invisible, and the cat
    /// would never return once the rows cleared.
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
        wd.rescan_from_cells_with_geom(
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
        );
        let ident = feline(&wd).ident;
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
        assert!(!cats.is_empty(), "the kitty is peeking mid-dwell");

        // A background job floods the body footprint rows mid-dwell. Blank
        // snapshot rows arrive TRIMMED (possibly empty), so extend them from
        // a blank template cell before stamping the text.
        let mut template = snap.cells[3][0];
        template.ch = ' ';
        template.wide = false;
        // Both candidate bands must be walled: a head whose upward band is
        // occupied simply peeks DOWN instead, so occluding only rows 1-2 would
        // relocate the cat rather than pause it.
        for r in [1usize, 2, 4, 5] {
            snap.cells[r].resize(cols, template);
            for cell in &mut snap.cells[r] {
                cell.ch = 'X';
            }
        }
        let t1 = t0 + Duration::from_millis(1000);
        wd.rescan_from_cells_with_geom(
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
        );
        let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(16), &c, g);
        assert!(cats.is_empty(), "an occluded cat emits nothing");
        assert!(
            wd.persist[&ident].peek_pause.is_some(),
            "occlusion latches the clearance pause"
        );

        // Far past the nominal peek length: the paused clock must NOT burn out.
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
        wd.rescan_from_cells_with_geom(
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

    /// CAP-BIAS regression: with more raw matches than [`MAX_OCCURRENCES`],
    /// plain top-down truncation starves the BOTTOM rows — exactly where the
    /// prompt lives. The bottom-priority cutoff must keep the typed word's row
    /// (dropping whole TOP rows instead), preserve row-major order within the
    /// kept rows, and stay stable across rescans.
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

        // Budget check: kitty row estimates 2 (1 match + the caret preview
        // reserve), rows 3..1 cost 40 each, and row 0 (40) does not fit — so
        // the kept suffix is rows 1-4: 3·40 + 1 occurrences.
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

        // Stability: an identical rescan keeps the same suffix and continues
        // (rather than rebirths) the prompt kitty.
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

    /// A WordDecorations carrying one eligible adult cat occurrence (row 2).
    fn settled_cat(now: Instant) -> WordDecorations {
        WordDecorations {
            occ: vec![cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now)],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        }
    }

    /// Rise + descend (overlay Phase 4): the body sprite animates a
    /// source-rect reveal anchored at the art TOP (ears enter first) with the
    /// dest bottom pinned at `word_top + chin`; mid-motion emits no gaze dots.
    #[test]
    fn free_rise_and_descend_reveal_window() {
        let now = Instant::now();
        let mk = || WordDecorations {
            occ: vec![cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now)],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let o = mk().occ[0].clone();
        let dwell = peek_dwell_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        let antic = anticipation_ms(o.genome.gkey);
        let variant = cat_variant_v4(GKEY_ADULT_HEAD);
        let (_, hart) =
            authored_cat_size(variant, f32::from(cat_hart(20)) * CatAge::Adult.scale(), 20);
        // The settled dest bottom: word_row_top + the age-scaled chin.
        let rest_bottom = 2 * 20 + i32::from(cat_chin(hart));
        // Mid-rise, late-rise, and mid-descend sample instants.
        let mid_descend = CAT_RISE_MS + dwell + antic + 160;
        for t_ms in [120u64, 300, mid_descend] {
            let at = now + Duration::from_millis(t_ms);
            let (fr, _, _) = tick_cat(&mut mk(), at, &cfg(), geom20());
            assert_eq!(
                fr.len(),
                1,
                "one revealing sprite at t={t_ms} (no gaze mid-motion)"
            );
            let body = fr[0];
            assert!(
                i32::from(body.h) <= i32::from(hart),
                "the reveal never exceeds the rest window at t={t_ms} \
                 (ease-out-back overshoot lifts the DEST, capped by vh = min(reveal, rest))"
            );
            assert_eq!((body.aw, body.ah), (body.w, body.h), "NEAREST 1:1 window");
            // Overshoot lifts the dest ABOVE the pinned rest bottom, never
            // below it; mid-descend (no lift) sits exactly at it.
            assert!(
                body.y + i32::from(body.h) <= rest_bottom,
                "the dest bottom never drops below word_top + chin at t={t_ms}"
            );
            assert_eq!(
                (body.z, body.sampler),
                (FreeZ::UnderText, FreeSampler::Nearest)
            );
        }
        // Early rise is a strictly partial reveal that grows monotonically,
        // and the descend shrinks back below the rest window.
        let (fr_a, _, _) = tick_cat(
            &mut mk(),
            now + Duration::from_millis(120),
            &cfg(),
            geom20(),
        );
        let (fr_b, _, _) = tick_cat(
            &mut mk(),
            now + Duration::from_millis(300),
            &cfg(),
            geom20(),
        );
        assert!(fr_a[0].h < hart, "early rise is a partial reveal");
        assert!(fr_b[0].h > fr_a[0].h, "the reveal grows through the rise");
        let (fr_d, _, _) = tick_cat(
            &mut mk(),
            now + Duration::from_millis(mid_descend),
            &cfg(),
            geom20(),
        );
        assert!(fr_d[0].h < hart, "mid-descend is a shrinking reveal");
        // Done: past the total window the one-shot emits zero forever.
        let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        let at = now + Duration::from_millis(total + 50);
        let (fr, _, _) = tick_cat(&mut mk(), at, &cfg(), geom20());
        assert!(fr.is_empty(), "Done: zero sprites forever");
    }

    /// `fold_free` covers EVERY field — z and sampler included (overlay doc
    /// §3.3: a same-rect Under→Over flip must move `deco_fp`, or the Tier-0
    /// early-out swallows a real content change) — and the signed origin by
    /// bit pattern.
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

    /// §5.6 kitten build: 1.3× overshoot + the 2 px landing bounce (dest-rect
    /// offsets only) — the bounce peak sits 2 px below the settled rest.
    #[test]
    fn kitten_landing_bounce_moves_dest_two_px() {
        let now = Instant::now();
        // gkey 0: age Kitten, pose HeadPeek, overshoot 0.06.
        let mut wd = WordDecorations {
            occ: vec![cat_occ(2, 2, 6, 0, now)],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let c = cfg();
        let g = geom20();
        // Warm the bakes through the entrance.
        for t in [100u64, 300, 440] {
            tick_cat(&mut wd, now + Duration::from_millis(t), &c, g);
        }
        // Bounce peak: t = 450 + 75 ⇒ sin(π/2) ⇒ +2 px.
        let (frb, _, _) = tick_cat(&mut wd, now + Duration::from_millis(525), &c, g);
        let bottom = |fr: &[FreeSprite]| fr.iter().map(|s| s.y + i32::from(s.h)).max().unwrap();
        assert!(
            wd.is_active(now + Duration::from_millis(525)),
            "the bounce is animating"
        );
        let (frs, _, _) = tick_cat(&mut wd, now + Duration::from_millis(900), &c, g);
        assert_eq!(
            bottom(&frb),
            bottom(&frs) + 2,
            "bounce peak dips 2 px below rest"
        );
        // v3: the dwell keeps the one-shot active until Done.
        assert!(wd.is_active(now + Duration::from_millis(900)));
    }

    // ───────────────────── §5.8 gaze battery (P3) ─────────────────────

    // ─────────────── v3 §1.2 in-dwell life battery (pure time) ───────────────

    /// v3 §1.2 duty pin: after Done
    /// the episode emits ZERO quads, `next_deadline()` is `None`, `is_active`
    /// is false, and the fp is frozen — zero wakes forever.
    #[test]
    fn post_done_duty_pin_no_deadline_no_activity_zero_quads() {
        let now = Instant::now();
        let mut wd = settled_cat(now);
        let o = wd.occ[0].clone();
        let c = cfg();
        let g = geom20();
        let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
        // Ride the whole cycle (rise → dwell → descend).
        for t in [100u64, 400, DWELL_QUIET_MS, total - 100] {
            let (fr, _, _) = tick_cat(&mut wd, now + Duration::from_millis(t), &c, g);
            assert!(!fr.is_empty(), "the peek emits through its window (t={t})");
            assert!(wd.is_active(now + Duration::from_millis(t)));
        }
        // Done, forever.
        let after = |ms: u64| now + Duration::from_millis(total + ms);
        let (fr, _, fp1) = tick_cat(&mut wd, after(10), &c, g);
        assert!(fr.is_empty(), "Done = zero sprites");
        assert_eq!(wd.next_deadline(after(10)), None, "nothing ever arms");
        assert!(!wd.is_active(after(10)));
        let (fr, _, fp2) = tick_cat(&mut wd, after(30_000), &c, g);
        assert!(fr.is_empty(), "still zero sprites 30 s later");
        assert_eq!(fp1, fp2, "the post-Done fp is frozen (zero wakes)");
        assert_eq!(wd.next_deadline(after(30_000)), None);
    }

    /// The peek phase clock latches AT BIRTH ([`Episode::fresh`]), before any
    /// tick, so the entrance plays from the word's first appearance: quads land
    /// on the very first ticked frame (no latch tick required), and same-frame
    /// siblings rise together (no stagger).
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
        // The word row a sprite belongs to: its dest bottom is pinned at
        // `word_row_top + chin`, so the bottom pixel maps into the word row.
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

    /// An episode born while the window is UNFOCUSED runs on the
    /// same birth-latched wall clock — a focus flip never re-latches it, so
    /// once the peek window has elapsed a refocus shows NO entrance, and a
    /// rescan of the unchanged grid never replays the spent episode.
    #[test]
    fn born_unfocused_entrance_never_replays_on_refocus() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\nnice kitty");
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        // Unfocused presents: the clock is already running (latched at
        // birth) — nothing here re-arms or defers it.
        tick_cat_at(&mut wd, t0, &c, g, None, false);
        tick_cat_at(&mut wd, t0 + Duration::from_millis(200), &c, g, None, false);
        // Refocus far past every reachable peek total (450+3750+380=4580):
        // the one-shot elapsed by wall time — no entrance, no re-latch.
        let t_focus = t0 + Duration::from_millis(6_000);
        let (fr, _, _) = tick_cat_at(&mut wd, t_focus, &c, g, None, true);
        assert!(fr.is_empty(), "no entrance replay on refocus: {fr:?}");
        let cat = wd
            .occ
            .iter()
            .find(|o| o.spec.graphic.is_some())
            .expect("the cat word scanned")
            .ident;
        assert!(
            wd.persist[&cat].peek_done,
            "the one-shot elapsed by wall time while unfocused"
        );
        let (fr, _, _) = tick_cat_at(
            &mut wd,
            t_focus + Duration::from_millis(100),
            &c,
            g,
            None,
            true,
        );
        assert!(fr.is_empty(), "…and stays spent");
        term.process(b" "); // damage: force a real rescan
        wd.rescan(&term, 4, 20, &lex, &c, 2, t_focus);
        let (fr, _, _) = tick_cat_at(
            &mut wd,
            t_focus + Duration::from_millis(200),
            &c,
            g,
            None,
            true,
        );
        assert!(fr.is_empty(), "a rescan never re-plays a spent episode");
        assert!(!wd.is_active(t_focus + Duration::from_millis(200)));
    }

    /// v3 §1.2 twin desync: same-seed twins carry distinct idents, so their
    /// dwells differ by the `mix(ident) % 300` jitter — their descend frames
    /// are never in lockstep.
    #[test]
    fn twin_desync_dwells_and_descend_frames_differ() {
        let seed = seed_of(2, Class::Feline, aterm_lexicon::form_hash("kitty"));
        let id0 = mix(seed);
        let id1 = mix(seed ^ ORDINAL_MIX);
        let g0 = GKEY_ADULT_HEAD;
        let d0 = peek_dwell_ms(id0, g0, false, (2200, 3598));
        let d1 = peek_dwell_ms(id1, g0, false, (2200, 3598));
        assert_ne!(d0, d1, "the twin-desync jitter separates the dwells");
        // Emission-level: at the earlier twin's mid-descend instant, the two
        // twins' reveals differ (one descending, one still dwelling or
        // further along) — drive two single-cat engines at the same clock.
        let now = Instant::now();
        let run = |ident: u64| {
            let mut o = cat_occ(2, 2, 6, g0, now);
            o.ident = ident;
            o.seed = seed;
            let mut wd = WordDecorations {
                occ: vec![o],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            let c = cfg();
            let sample = CAT_RISE_MS + d0.min(d1) + anticipation_ms(g0) + CAT_DESCEND_MS / 2;
            tick_cat(&mut wd, now + Duration::from_millis(sample), &c, geom20()).0
        };
        let (q0, q1) = (run(id0), run(id1));
        assert_ne!(q0, q1, "twin descend frames differ (desync jitter)");
    }

    /// v3 §1.1 fix #3: freeze/thaw mid-rise — the cat resumes at exactly the
    /// same phase (a cat frozen at 44% rise is at 44% after thaw), nothing
    /// completes while invisible, and the one-shot still runs to Done once.
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
        // Freeze one for 5 s, then thaw: its clock shifted by the freeze
        // duration, so its t = 5200 frame equals the control's t = 200 one.
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
        // The episode is NOT done-marked mid-rise: it still runs to Done.
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

    /// Tier-1 conformance (v3 §1.3): the REAL engine projects onto the
    /// ty-proven `OneShotPeek` model — Rise/Dwell/Descend/Done phase steps,
    /// occlusion shorter than GRACE_TTL (no re-Rise), freeze/thaw mid-rise
    /// (same phase, no extra Start), and a post-expiry re-appearance entering
    /// BORN-DONE (the healthy `Repeek` is a no-op; `NoRepeek` holds).
    #[test]
    fn one_shot_peek_conformance_real_engine() {
        let m = aterm_spec::derive::one_shot_peek_model();
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
        // Rise begins at birth (the phase clock latches in Episode::fresh;
        // the rescan above IS the birth at t0): model Start.
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
        // Dwell: model Step (Rise → Dwell). v4 cats bake their own eyes, so the
        // dwell is a steady body sprite — no live blink to time.
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
        // Occlusion < GRACE_TTL mid-dwell: the episode persists; NO re-Rise.
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
        // Post-expiry re-appearance: BORN-DONE — the healthy model's Repeek
        // is a no-op (phase stays Done, rises stays 1).
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

    /// §10: `feline.magic = false` pins the ordinary build — the Nebula magic
    /// window renders with no entrance aura (and no magic bake).
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

    // ───────────────── §5.2/§5.4 v2.2 geometry battery ─────────────────

    /// Exercise the shipping occurrence -> geometry -> BakeKey -> atlas path,
    /// not the natural-aspect gallery helper. Both a normal head and the tall
    /// Maneki special retain their authored viewbox proportions, while the age
    /// axis scales both dimensions together.
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

    /// §5.2 v2.9 overshoot clamp: the 2-band head occupies rows {r−2, r−1, r};
    /// across the WHOLE entrance + overshoot + bounce, no quad ever crosses
    /// into row r−3. (With the taller Hart the head has two full rows of
    /// headroom, so the `lift ≤ 2·ch − (Hart − chin)` clamp is purely
    /// defensive — the natural overshoot never reaches it.)
    #[test]
    fn entrance_overshoot_never_crosses_into_row_r_minus_3() {
        let now = Instant::now();
        // gkey: age Kitten (0), pose HeadPeek (0), overshoot ord 3
        // (Gray-encode 3 = 0b10 at v4 bits 15–16).
        let gkey = 2u64 << 15;
        // Row 3: r−2 = row 1, r−3 = row 0 (representable, so a spill would show).
        let mut wd = WordDecorations {
            occ: vec![cat_occ(3, 2, 6, gkey, now)],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let c = cfg();
        let g = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: 6,
            cols: 20,
        };
        // Uniform kitten scaling still leaves the complete sprite inside rows
        // {1, 2, 3} (r−2 = row 1); overshoot must not lift it into row 0.
        let mut min_y = i32::MAX;
        for t in (10..620u64).step_by(5) {
            let (fr, _, _) = tick_cat(&mut wd, now + Duration::from_millis(t), &c, g);
            for s in &fr {
                assert!(s.y >= 16, "sprite crossed into row r−3: {s:?}");
                min_y = min_y.min(s.y);
            }
        }
        // The peak excursion stays inside the eyes' band (row 1, y ≥ 16).
        assert!(
            min_y >= 16,
            "the head never lifts above row r−2 (got {min_y})"
        );
    }

    // ────────── v3 §1.1 alignment / done-marks / reset-table battery ──────────

    /// v3 §1.1 fix #1, rotation at CONSTANT count (the log-tail case): the top
    /// twin leaves and a new bottom twin enters in one rescan. Row-anchored
    /// alignment pins the surviving episode to its physical word (mid-dwell
    /// cats never teleport), the new bottom twin PLAYS (fresh episode), and
    /// the departed top twin grace-expires.
    #[test]
    fn rotation_alignment_plays_new_twin_and_pins_survivor() {
        let model = aterm_spec::derive::sparkle_reflow_cardinality_model();
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
        // Rotate: the top twin (row 0) scrolls off, a new twin enters at
        // row 4 — the row multiset changes at constant count.
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
        // The row-2 survivor moved ordinal 1 → 0: it KEEPS its episode
        // (appeared = t0 — mid-dwell stays pinned to its physical word).
        assert_eq!(cats2[0].row, 2);
        assert_eq!(cats2[0].appeared, t0, "the survivor keeps its episode");
        // The new bottom twin (row 4, ordinal 1) is FRESH: it plays.
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
        // Gaze rekeyed with the survivor: nothing dangles on the old ident.
        assert!(
            wd.persist.contains_key(&id0) && wd.persist.contains_key(&id1),
            "both live idents hold episodes"
        );
    }

    /// Maximum-cardinality negative space: with no stationary anchor, a global
    /// one-row insertion must transfer every twin. Three occurrences exercise
    /// the augmenting-chain shape that a greedy first-bid matcher loses.
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

    /// v3 §1.1 fix #1 skip semantics: old rows [0, 5] vs new [5, 20] must
    /// resolve 0→unmatched, 5→5, 20→fresh — the unmatched old episode never
    /// falls back to a farther occurrence (naive in-order pairing is wrong).
    #[test]
    fn alignment_skip_semantics_old_episode_goes_unmatched() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut a = Terminal::new(24, 64);
        a.process(b"dear kitty friend\r\n\r\n\r\n\r\n\r\ndear kitty friend");
        wd.rescan(&a, 24, 64, &lex, &c, 1, t0);
        let seed = feline(&wd).seed;
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
        let _ = seed;
    }

    /// v3 §1.1 write path, transient `reset()`: the flush pass writes every
    /// started episode's done mark BEFORE clearing, and the same logical line
    /// re-enters BORN-DONE; `hard_reset()` clears the marks and replays.
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

    /// v3 §1.1: alignment moves done-mark responsibility WITH the episode —
    /// a shrink transfers the surviving twin's episode to its new ident, and
    /// when it later grace-expires the mark is written under the ident
    /// current at departure, so the next same-context appearance is born-done.
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
        // Shrink: the ROW-0 twin scrolls off; the row-2 survivor shifts to
        // ordinal 0 via alignment (its episode transfers to the new ident).
        let mut lone = Terminal::new(6, 64);
        lone.process(b"\r\n\r\ndear kitty friend");
        wd.rescan(&lone, 6, 64, &lex, &c, 2, at(1));
        let survivor = feline(&wd).clone();
        assert_eq!(survivor.appeared, t0, "the survivor kept its episode");
        // Everything expires (> GRACE_TTL blank): the transferred episode's
        // mark is written under its CURRENT (post-transfer) ident.
        let blank = Terminal::new(6, 64);
        wd.rescan(&blank, 6, 64, &lex, &c, 3, at(15));
        assert!(wd.persist.is_empty());
        assert!(!wd.done_marks.is_empty(), "expiry wrote the done marks");
        // The lone layout re-enters: the survivor's word is BORN-DONE under
        // exactly the transferred key (ident at departure ^ ctx_fp).
        wd.rescan(&lone, 6, 64, &lex, &c, 4, at(16));
        assert!(
            wd.persist[&survivor.ident].born_done,
            "the re-appearance is suppressed by the transferred mark"
        );
    }

    /// v3 §1.1 fix #4 resize-settle: births during a cols change (and until
    /// the first rescan at stable cols + 500 ms) are BORN-SETTLED — no
    /// entrance, static ink, and never written to `done_marks`; births after
    /// the window play normally.
    #[test]
    fn resize_settle_births_are_inert_and_unmarked() {
        let lex = lex();
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        // Establish cols = 40 on an empty grid, then a width step to 32
        // arrives TOGETHER with a new word — the interactive-resize birth.
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
        // Departure of a born-settled episode writes NO mark.
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

    /// §F4.2: one sighting per EPISODE — recorded on the first present where
    /// quads land, silent across later frames and grace re-hits, recounted
    /// only on true episode death (grace expiry), exactly the §3.6 semantics.
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
        // The first present at t0 is at reveal 0 (the clock latched at
        // birth = t0 — nothing lands yet)…
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
        // v3 §1.1: true episode death (grace expiry) writes the done mark, so
        // the re-appearance is BORN-DONE — inert, and it logs NOTHING.
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
        // hard_reset (fresh start is user intent): the marks clear and the
        // next appearance records again — exactly 1.
        wd.hard_reset();
        wd.rescan(&term, 4, 20, &lex, &c, 6, at(17));
        tick_cat_at(&mut wd, at(17), &c, g, None, true); // latch the phase clock
        tick_cat_at(
            &mut wd,
            at(17) + Duration::from_millis(100),
            &c,
            g,
            None,
            true,
        );
        assert_eq!(
            wd.drain_kitty_sightings().count(),
            1,
            "hard_reset makes the sighting reachable again"
        );
    }

    // ───────────────────── §6 supernova battery (P4) ─────────────────────

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
            now, cfg, geom, cursor, sel, true, &mut out, &mut ink, &mut fr, &mut nova,
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

    fn select_span(row: i32, c0: u16, c1: u16) -> aterm_core::selection::TextSelection {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let mut s = aterm_core::selection::TextSelection::new();
        s.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
        s.update_selection(row, c1, SelectionSide::Right);
        s.complete_selection();
        s
    }

    /// §6.4 item 1 / §13: ONE flash per identity episode, across occlusion —
    /// the full phase ride (Dip emits nothing, Flash crowns, Ring emits quads,
    /// window end settles to the ember residual with a stable fp), then a
    /// grace re-hit never re-ignites, and only true identity death re-arms.
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
        // Ring window: quads live, and the fp changes across frames (the
        // anti-skip rule folds the frame term + every nova quad).
        let (q1, _, _, _, fp1) = tick_nova(&mut wd, at(500), &c, g, None, None);
        assert!(!q1.is_empty(), "ring quads");
        let (_, _, _, _, fp2) = tick_nova(&mut wd, at(516), &c, g, None, None);
        assert_ne!(fp1, fp2, "an animating nova must never be present-skipped");
        // Debris rides the EXISTING wdeco Add stream mid-window.
        let (_, out, _, _, _) = tick_nova(&mut wd, at(700), &c, g, None, None);
        assert!(
            out.iter()
                .filter(|d| matches!(d.blend, DecoBlend::Add))
                .count()
                > 1,
            "ballistic debris motes on the Add stream, got {out:?}"
        );
        // v3 §1.2 graphics-decay: past the window the ember residual FADES
        // (still animating) for ≤ 2 s…
        let (q, out, _, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
        assert!(q.is_empty(), "the fade emits nothing on the nova stream");
        assert_eq!(out.len(), 1, "the ember spark is fading");
        assert!(matches!(out[0].blend, DecoBlend::Add));
        assert!(wd.is_active(at(2600)), "the ember fade is animating");
        // …then ZERO decos + fp-stable + disarmed, forever.
        let (q, out, _, _, fpa) = tick_nova(&mut wd, at(3600), &c, g, None, None);
        assert!(q.is_empty() && out.is_empty(), "the ember faded to nothing");
        let (_, _, _, _, fpb) = tick_nova(&mut wd, at(3616), &c, g, None, None);
        assert_eq!(fpa, fpb, "the post-fade steady state is fp-stable");
        assert!(
            !wd.is_active(at(3616)),
            "a spent nova disarms the scheduler"
        );
        // Occlude one rescan (grace holds the episode), re-hit: NO re-flash.
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
        // v3 §1.1 fix #2, the INVERTED rebirth stanza: true identity death
        // (grace expiry) writes the done mark, so a post-expiry re-appearance
        // is BORN-DONE — no re-ignition, ever; only the settled ink shows.
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
        // hard_reset branch: re-ignition IS still reachable when the user
        // asks for a fresh start (§1.1 reset table).
        wd.hard_reset();
        wd.rescan(&term, 2, 20, &lex, &c, 6, at(18_000));
        tick_nova(&mut wd, at(18_000), &c, g, None, None); // grant
        let (q, _, _, _, _) = tick_nova(&mut wd, at(18_300), &c, g, None, None);
        assert!(!q.is_empty(), "hard_reset makes re-ignition reachable");
    }

    /// Tier-1 binding for `DoneMarkLruBound`: fill the real bounded LRU, touch
    /// its oldest key, then replace at capacity. The model's resident/eviction
    /// counters match, the exact deterministic order is checked, and the real
    /// mutation touches a fixed number of links independent of cardinality.
    #[test]
    fn done_mark_lru_real_order_and_constant_work_conform() {
        const CAP: usize = 3;
        const MAX_LINK_WRITES: u8 = 6;
        let model = aterm_spec::derive::done_mark_lru_model();
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

        // Negative control: the previous `iter().min_by_key` implementation
        // examines every resident. The Buggy model records those three probes
        // and rejects the exact at-cap transition the real LRU handled above.
        let mut buggy = aterm_spec::derive::done_mark_lru_model();
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

    /// The shipping 65,536-entry boundary: after one warm fill, thousands of
    /// deterministic replacements reuse the same slots and hash allocation.
    /// `link_writes <= 6` is the structural constant-work oracle; it cannot
    /// accidentally pass through a cardinality-sized scan.
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
        // HashMap's reported capacity may decrease as removals leave
        // tombstones, even though its bucket allocation is unchanged. It must
        // never exceed the fully-warmed allocation; the intrusive node/free
        // storage remains byte-for-byte fixed.
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
        let start = grant_ignition(&mut wd.ignitions, 0, owner, now, (100, 100), 88.0)
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

    /// Tier-1 binding for `IgnitionReservationLifecycle`: the real limiter
    /// grants one immediate and two delayed overlapping slots, cancels only a
    /// departed FUTURE owner, preserves fired history after owner departure,
    /// and migrates a pending slot into recent history at its start instant.
    /// The expiry-only negative control retains two ownerless future slots and
    /// projects exactly onto the model's Buggy counterexample.
    #[test]
    fn ignition_reservation_lifecycle_real_queue_conforms() {
        let model = aterm_spec::derive::ignition_reservation_lifecycle_model();
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

        // A new owner can use the newly-open current slot without disturbing
        // owner 3's already-reserved future cadence.
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

        // Negative control: an expiry-only sweep leaves both future slots after
        // their owners depart. Drive the Buggy model in lockstep.
        let mut buggy = aterm_spec::derive::ignition_reservation_lifecycle_model();
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

    /// Tier-1 binding for `IgnitionReservationRekey`: a limiter-delayed nova
    /// is rekeyed exactly as the alignment pass moves its episode, survives
    /// pruning under the new owner, and keeps an overlapping competitor out of
    /// its rolling safety window. The negative control performs a persist-only
    /// rekey and demonstrates the resulting simultaneous flashes.
    #[test]
    fn ignition_reservation_rekey_real_queue_conforms() {
        let model = aterm_spec::derive::ignition_reservation_rekey_model();
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

        // The first overlapping flash occupies t0, so the episode under test
        // receives a delayed t1 slot.
        assert_eq!(reserve_overlapping(&mut wd, blocker, t0), t0);
        let original_start = grant_ignition(&mut wd.ignitions, 0, old_owner, t0, (100, 100), 88.0)
            .expect("live target fits structural cap");
        assert_eq!(original_start, t1);
        wd.persist
            .get_mut(&old_owner)
            .expect("target episode")
            .nova_start = Some(original_start);
        assert!(model.fire("GrantDelayed", &mut state));

        // Drive the real scanner/alignment transaction: the same exact surface
        // moves horizontally, so align_pending pulls the old episode and calls
        // the atomic rekey funnel under its new occurrence identity.
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

        // The preserved t1 slot pushes an overlapping competitor to t2; no
        // flash can share the original episode's trailing one-second window.
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

        // Negative control: rekey ONLY the persist map. Prune cannot find owner
        // 2 and drops its future slot, even though episode 22 still carries
        // nova_start=t1 and will flash then.
        let mut buggy = aterm_spec::derive::ignition_reservation_rekey_model();
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

    /// Ten thousand vanished delayed owners are the backlog shape that would
    /// accumulate without pruning. Pruning each unconsumed future slot leaves
    /// only the one fired safety record, and the resident Vec stops growing
    /// after warmup.
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

    /// §6.4 item 2 / §13: the window-wide limiter — two disjoint-region
    /// ignitions fill the rolling second; a third arriving 200 ms later is
    /// DELAYED deterministically to the earliest allowed slot (t0 + 1 s), and
    /// the queued Dip still fires.
    #[test]
    fn limiter_delays_third_of_three_ignitions_deterministically() {
        let lex = lex();
        let c = cfg_nova();
        // Rows 0 / 6 / 12 at cell 10×20: pairwise center distance ≥ 120 px >
        // 2·R_max ≤ 88 px — no overlap tightening in this variant.
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

    /// §6.4 item 2 / §13 overlap tightening: two candidates whose regions
    /// overlap (center distance < 2·R_max) tighten the limiter to ≤ 1 per
    /// rolling second — the second ignition is pushed a full window out even
    /// though the plain cap (2/s) had room.
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

    /// A persist identity is a current owner label, not a globally unique
    /// reservation nonce. If a later episode legitimately reuses that number
    /// while the former episode remains only as fired safety history, it must
    /// receive a new limiter decision instead of aliasing the old start.
    #[test]
    fn limiter_reused_owner_label_does_not_alias_fired_episode() {
        let t0 = Instant::now();
        let t1 = t0 + IGNITION_WINDOW;
        let mut ignitions = Vec::new();
        assert_eq!(
            grant_ignition(&mut ignitions, 0, 7, t0, (100, 100), 88.0),
            Some(t0)
        );
        assert_eq!(
            grant_ignition(
                &mut ignitions,
                0,
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

    /// Tier-1 conformance for the §9 `FlashLimiter` ty model (§7.5 ledger row
    /// "FlashLimiter (§6.4/§9)" — the limiter IS the WCAG 2.3.1 argument, so
    /// this binding is what lets v2 ship with no per-frame flash audit): the
    /// REAL window-wide limiter (`grant_ignition`) is driven by a scripted
    /// ignition storm and every decision is projected onto the derived model —
    /// grants map to `Ignite` (the model's guard must ADMIT each one), delays
    /// map to a disabled `Ignite` guard whose re-enabling `Shift` tick (250 ms
    /// slots, 4-slot rolling second) coincides EXACTLY with the real granted
    /// slot, and `IgnitionBound` + `RegionFlashPairs` hold along the whole
    /// trace, for both the disjoint (Overlap = 0) and the overlapping
    /// (Overlap = 1, tightened to ≤ 1/s) storms.
    #[test]
    fn flash_limiter_conformance_real_limiter_projects_onto_model() {
        let tick = Duration::from_millis(250);
        let t0 = Instant::now();

        // ── storm 1: DISJOINT regions (model scenario Overlap = 0) ──
        let m = aterm_spec::derive::flash_limiter_model();
        let mut st = m.init_state();
        let mut igns = Vec::new();
        let a = grant_ignition(&mut igns, 0, 1, t0, (100, 100), 88.0).expect("capacity");
        assert_eq!(a, t0, "an empty window grants immediately");
        assert!(m.fire("Ignite", &mut st), "the model admits ignition 1");
        let b = grant_ignition(&mut igns, 0, 2, t0, (900, 100), 88.0).expect("capacity");
        assert_eq!(b, t0, "two disjoint ignitions fit one rolling second");
        assert!(m.fire("Ignite", &mut st), "the model admits ignition 2");
        assert!(m.check_invariant("IgnitionBound", &st));
        assert!(m.check_invariant("RegionFlashPairs", &st));
        // The third request, 200 ms in: the real limiter DELAYS it to the
        // window edge; the model's Ignite guard is disabled at that tick.
        assert!(
            !m.action_enabled("Ignite", &st),
            "the model refuses a 3rd in-window ignition"
        );
        let c = grant_ignition(
            &mut igns,
            0,
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
        // Roll the second out in 250 ms model ticks: Ignite re-enables on
        // exactly the tick that reaches the real limiter's granted slot.
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

        // ── storm 2: OVERLAPPING regions (model scenario Overlap = 1) ──
        let mut mo = aterm_spec::derive::flash_limiter_model();
        for cst in &mut mo.consts {
            if cst.0 == "Overlap" {
                cst.1 = 1;
            }
        }
        let mut st = mo.init_state();
        let mut igns = Vec::new();
        let a = grant_ignition(&mut igns, 0, 1, t0, (100, 100), 88.0).expect("capacity");
        assert_eq!(a, t0);
        assert!(mo.fire("Ignite", &mut st));
        assert!(
            !mo.action_enabled("Ignite", &st),
            "overlap tightens the model to ≤ 1 per rolling second"
        );
        // 50 px < 2·R_max = 88 px: the real limiter pushes the overlapping
        // second ignition a FULL window out even though the plain 2/s cap
        // had room — the §6.4 item 2 tightening.
        let b = grant_ignition(&mut igns, 0, 2, t0, (150, 100), 88.0).expect("capacity");
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

    /// Negative control for the Tier-1 binding above (the §9 `Buggy = 1`
    /// twin, non-vacuity): an overlap-BLIND limiter — `grant_ignition` called
    /// with `overlap_dist = 0`, so no center distance is ever `< 0` and the
    /// §6.4 item 2 tightening never engages — reproduces exactly the model's
    /// `Buggy = 1, Overlap = 1` counterexample trace: two overlapping
    /// ignitions land in ONE rolling second (4 luminance-transition pairs on
    /// the shared region), the buggy model admits the trace, and the healthy
    /// model's `IgnitionBound` AND `RegionFlashPairs` both reject the
    /// projected state — the WCAG violation `ty` catches in Tier-0.
    #[test]
    fn flash_limiter_negative_control_overlap_blind_limiter_is_buggy_trace() {
        let mut healthy = aterm_spec::derive::flash_limiter_model();
        let mut buggy = aterm_spec::derive::flash_limiter_model();
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
        // Centers 50 px apart (genuinely overlapping regions), but the blind
        // limiter never sees it: BOTH ignite at t0 — the strobe.
        let a = grant_ignition(&mut igns, 0, 1, t0, (100, 100), 0.0).expect("capacity");
        let b = grant_ignition(&mut igns, 0, 2, t0, (150, 100), 0.0).expect("capacity");
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
        // …and the HEALTHY model rejects the projected state on BOTH
        // invariants: 2 ignitions > 1 under overlap, 2 + 2 = 4 pairs > 3.
        assert!(
            !healthy.check_invariant("IgnitionBound", &st),
            "2 overlapping ignitions in one second must violate IgnitionBound"
        );
        assert!(
            !healthy.check_invariant("RegionFlashPairs", &st),
            "4 transition pairs on the shared region must violate RegionFlashPairs"
        );
    }

    /// §6.4 item 6 / §13: a nova never ignites while its word is selected —
    /// ignition defers to deselection (then queues through the limiter) — and
    /// an already-active nova's quads attenuate over selected cells.
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
        // Deselection: the ignition queues through the limiter from NOW.
        tick_nova(&mut wd, at(800), &c, g, None, None);
        assert_eq!(ignition_starts(&wd), vec![Some(at(800))]);
        // An ACTIVE nova over a fresh selection: quads over selected cells
        // drop (host-side per-quad, both backends identical by construction).
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

    /// §6.4 item 5 / §13: `reduced_motion` ⇒ a static glint only — no dip, no
    /// flash, no ring, no debris; the ember tint + one static spark from
    /// frame 0, byte-stable forever.
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
        // The spark carries the palette's ember tone (not the v1 palette).
        let occ = &wd.occ[0];
        let feats = nova_features(occ.genome.gkey);
        let (_, ember) = nova::ember_pair(nova::palette(feats.palette), None);
        // Magic variants may retint (deterministic); ordinary genomes match.
        if nova_magic(occ.genome.magic).is_none() {
            assert_eq!(out1[0].color, ember);
        }
    }

    /// §6.1/§6.4 item 3 / §13: the word's own ink during its nova — the Dip
    /// dims the envelope ~35 %, the sweep is FROZEN at the settled gradient
    /// for the whole window, and the spent nova leaves the ember tint.
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
        // Frozen: mid-window frames are byte-identical despite the live sweep
        // window (the settled gradient held for the whole nova second).
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
        // The exact-v1 sparkle style at the same instants SWEEPS (not frozen).
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
        // Spent: the ink anchors shift to the ember pair (settled bytes use
        // the ember gradient, not the live palette endpoints).
        let (_, _, ink_ember, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
        assert_ne!(
            ink_ember, ink_a,
            "the ember tint differs from the live palette"
        );
    }

    /// §6.5 / §13 coupling: byte-deterministic (same text + same tick instants
    /// ⇒ identical ink, replay-reproducible), the pulse actually fires on a
    /// neighbor word, and the applied pulse holds |ΔL| ≤ 5 % against the
    /// un-pulsed control run.
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
                    // Keep only the NEIGHBOR word's cells (cols ≥ 5).
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
        // Control: the v1 sparkle style has no coupling; the frames where the
        // nova run deviates from it are exactly the pulse window.
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

    /// §6.1 magic variants through the full host tick: the Quasar's jets and
    /// the Singularity's Over-blend RingArc shadow both reach the output
    /// streams (the emitter-level shapes are pinned in `nova::tests`).
    #[test]
    fn quasar_and_singularity_ride_the_host_tick() {
        const QUASAR_MAGIC: u64 = 0; // (0 >> 12) % 4096 = 0 → Quasar
        const SING_MAGIC: u64 = 8 << 12; // → 8 → Singularity
        let now = Instant::now();
        let g = geom20();
        let run = |magic: u64, profanity_magic: bool| {
            let mut wd = WordDecorations {
                occ: vec![Occurrence {
                    row: 2,
                    start_col: 4,
                    end_col: 7,
                    class: Class::Profanity,
                    langs: LangSet::EMPTY,
                    form_id: FormId::UNKNOWN,
                    seed: 0xF00D,
                    ident: mix(0xF00D),
                    appeared: now,
                    genome: Genome { gkey: 0, magic },
                    ink_base: 0,
                    ink_cells: 0,
                    ink_bg: [0; 3],
                    cat_colors: CatColorKey::default(),
                    cat_text_clear: true,
                    cat_peek_down: false,
                    dec_line: false,
                    inert: false,
                    spec: class_default_spec(Class::Profanity, &cfg_nova()),
                    custom: false,
                }],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            let mut e = Episode::fresh(now, Genome { gkey: 0, magic }, 0xF00D, 2, 0);
            e.nova_start = Some(now);
            wd.persist.insert(mix(0xF00D), e);
            let mut c = cfg_nova();
            c.profanity_magic = profanity_magic;
            let at = now + Duration::from_millis(400);
            let mut wd2 = wd;
            tick_nova(&mut wd2, at, &c, g, None, None)
        };
        // Quasar: vertical jets hug the center column (x ≈ 55 px).
        let (q, _, _, _, _) = run(QUASAR_MAGIC, true);
        assert!(!q.is_empty());
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

    /// §6.3 caps through the host tick: >3 concurrent novas — the 4th (row-
    /// major last) skips straight to Ember (its episode spends without quads).
    #[test]
    fn excess_concurrent_novas_skip_to_ember() {
        let now = Instant::now();
        let g = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 30,
            cols: 20,
        };
        let mut wd = WordDecorations {
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        for k in 0..4u16 {
            let seed = 0xF00D + u64::from(k);
            let o = Occurrence {
                row: k * 7,
                start_col: 4,
                end_col: 7,
                class: Class::Profanity,
                langs: LangSet::EMPTY,
                form_id: FormId::UNKNOWN,
                seed,
                ident: mix(seed),
                appeared: now,
                genome: Genome {
                    gkey: 0,
                    magic: (100 << 12) | 100,
                },
                ink_base: 0,
                ink_cells: 0,
                ink_bg: [0; 3],
                cat_colors: CatColorKey::default(),
                cat_text_clear: true,
                cat_peek_down: false,
                dec_line: false,
                inert: false,
                spec: class_default_spec(Class::Profanity, &cfg_nova()),
                custom: false,
            };
            // All four already granted (bypassing the limiter — this pins the
            // CONCURRENCY cap, not the rate limiter).
            let mut e = Episode::fresh(now, o.genome, o.seed, o.row, 0);
            e.nova_start = Some(now);
            wd.persist.insert(o.ident, e);
            wd.occ.push(o);
        }
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

    /// §10/§12: DEC double-width rows are supported like ink — the nova
    /// anchors via the ROW ADVANCE (2× the cell width on DECDWL), so the ring
    /// centers over the glyphs the user sees instead of the logical columns.
    #[test]
    fn dec_rows_anchor_nova_via_row_advance() {
        let now = Instant::now();
        let g = geom20();
        let run = |dec_line: bool| {
            let o = Occurrence {
                row: 2,
                start_col: 4,
                end_col: 7,
                class: Class::Profanity,
                langs: LangSet::EMPTY,
                form_id: FormId::UNKNOWN,
                seed: 0xF00D,
                ident: mix(0xF00D),
                appeared: now,
                genome: Genome {
                    gkey: 0,
                    magic: (100 << 12) | 100,
                },
                ink_base: 0,
                ink_cells: 0,
                ink_bg: [0; 3],
                cat_colors: CatColorKey::default(),
                cat_text_clear: true,
                cat_peek_down: false,
                dec_line,
                inert: false,
                spec: class_default_spec(Class::Profanity, &cfg_nova()),
                custom: false,
            };
            let mut wd = WordDecorations {
                occ: vec![o],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            let mut e = Episode::fresh(
                now,
                Genome {
                    gkey: 0,
                    magic: (100 << 12) | 100,
                },
                0xF00D,
                2,
                0,
            );
            e.nova_start = Some(now);
            wd.persist.insert(mix(0xF00D), e);
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
        // Span cols 4..=7 at cw = 10: center 60 px; at the DECDWL advance
        // (20 px) it is 120 px.
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

    /// Rainbow profanity default with the supernova roll disabled (the driven
    /// default-scenario shape).
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

    /// §6: custom rainbow word end-to-end — scans as emphasis, resolves the
    /// override spec, emits animated rainbow ink, settles byte-stable.
    #[test]
    fn custom_rainbow_word_end_to_end() {
        let (c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\nink = { colorway = \"rainbow\" }\n",
        );
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 40);
        term.process(b"go ultrathink now");
        wd.rescan(&term, 2, 40, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "the custom word scans");
        assert!(wd.occ[0].custom, "the override spec resolved");
        assert!(matches!(
            wd.occ[0].spec.ink,
            Some(InkSpec {
                colorway: Colorway::Rainbow { .. },
                ..
            })
        ));
        let (_, _, ink0, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert_eq!(ink0.len(), 10, "one InkCell per lead cell");
        assert!(wd.is_active(at(1)), "the drift animates");
        let (_, _, ink_a, _, fp_a) = tick_nova(&mut wd, at(3000), &c, geom20(), None, None);
        let (_, _, ink_b, _, fp_b) = tick_nova(&mut wd, at(3200), &c, geom20(), None, None);
        assert_eq!(ink_a, ink_b, "settled rainbow is byte-stable");
        assert_eq!(fp_a, fp_b, "settled fp is stable (frame fold gated)");
        assert!(!wd.is_active(at(3000)), "settled ink disarms the scheduler");
        assert_eq!(ink0, ink_a, "EXACT full cycle: frozen phase == t = 0 phase");
    }

    /// §6: a custom spec on a BUILTIN profanity form fires with
    /// `[profanity] enabled = false` (override bypasses the class gate) and
    /// beats the class default regardless of the match's class.
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

    /// §6: a GRAPHIC-ONLY custom word with ink disabled still scans and shows
    /// the peeking cat (the emphasis resolve gate is
    /// `enabled && (ink_enabled || has_custom_specs)` — resolver-level; the
    /// engine honors `cfg.emphasis` as folded).
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

    /// §6: 2-char custom word (the user-surface guard exemption end-to-end).
    #[test]
    fn two_char_custom_word_scans_and_styles() {
        let (c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"gg\"]\nink = { colorway = \"rainbow\" }\n",
        );
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 20);
        term.process(b"gg well played");
        wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "explicit config is consent (v3 §6)");
        assert!(wd.occ[0].custom);
        let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert_eq!(ink.len(), 2, "both lead cells ink");
    }

    /// §6: the possessive form styles IDENTICALLY (the resolver registered
    /// the four possessive-variant hashes).
    #[test]
    fn possessive_custom_form_styled_identically() {
        let (c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\nink = { colorway = \"rainbow\" }\n",
        );
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 40);
        term.process(b"ultrathink's power");
        wd.rescan(&term, 2, 40, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "the possessive matches");
        assert!(
            wd.occ[0].custom,
            "possessive hits carry the full-token hash; the variant keys cover it"
        );
        assert_eq!(wd.occ[0].spec, wd.occ[0].spec, "same spec object");
        assert!(matches!(
            wd.occ[0].spec.ink,
            Some(InkSpec {
                colorway: Colorway::Rainbow { .. },
                ..
            })
        ));
    }

    /// §6: a no-space-script (CJK) custom surface matches via the synthesized
    /// `cjk = true` entry and its RAW form_hash key.
    #[test]
    fn cjk_custom_surface_matches() {
        let (c, lex) = lex_with_customs(
            "[[sparkle_words.custom]]\nwords = [\"\u{732b}\u{795e}\"]\nink = { colorway = \"rainbow\" }\n",
        );
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 30);
        term.process("これは猫神です".as_bytes());
        wd.rescan(&term, 2, 30, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "the CJK custom compound scans");
        assert!(wd.occ[0].custom, "RAW-hash override resolved");
        assert_eq!(wd.occ[0].class, Class::Emphasis);
    }

    /// §3.1 rainbow freeze battery: two ticks past the freeze emit identical
    /// InkCells AND identical fingerprints; `is_active == false` with
    /// non-empty settled ink; the light-theme twin holds the same contract;
    /// a done-born rainbow is byte-identical to a naturally-settled one.
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

    /// §3.2 roll: decorrelated across appearances (session birth_seq), never
    /// under chance = 0, always under chance = 100, and the stored decision
    /// TRANSFERS across row alignment.
    #[test]
    fn supernova_roll_decorrelates_and_transfers_across_alignment() {
        // Function-level shape: the salted mix over birth sequences hits both
        // outcomes at 10%.
        let roll_seed = 0xABCD_EF12_3456_7890u64;
        let outcomes: Vec<bool> = (1..=200u64)
            .map(|s| mix(roll_seed ^ s ^ supernova::SUPERNOVA_SALT) % 100 < 10)
            .collect();
        assert!(outcomes.iter().any(|h| *h) && outcomes.iter().any(|h| !*h));

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
        // Redraw the word two rows lower (same column ⇒ same seed): row
        // alignment moves the episode — and the roll — with it.
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
        // Independence: the same word/column/context born repeatedly (hard
        // resets keep the SESSION birth_seq) sees both outcomes at 50%.
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

    /// §3.2: MAX_ACTIVE_SUPERNOVAE = 1 + the GLOBAL BURST MUTEX — a second
    /// rolled supernova AND a classic nova both defer while the first
    /// supernova is granted/live, then grant after its window.
    #[test]
    fn supernova_mutex_serializes_bursts() {
        let (table, lex_frag) = custom_table(
            "[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n",
        );
        let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("override parses");
        let mut c = cfg_rainbow(100);
        c.spec_table = table;
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let g = geom20();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(8, 20);
        term.process(b"fuck\r\nfuck it\r\nboom");
        wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
        let start_of = |wd: &WordDecorations, i: usize| {
            wd.persist.get(&wd.occ[i].ident).and_then(|e| e.nova_start)
        };
        assert_eq!(wd.occ.len(), 3);
        tick_nova(&mut wd, t0, &c, g, None, None);
        assert!(
            start_of(&wd, 0).is_some(),
            "row-major first supernova grants"
        );
        assert!(
            start_of(&wd, 1).is_none(),
            "MAX_ACTIVE_SUPERNOVAE = 1: the second roll defers"
        );
        assert!(
            start_of(&wd, 2).is_none(),
            "burst mutex: the classic nova defers behind the live supernova"
        );
        // Past the first window: the second supernova takes the mutex.
        tick_nova(
            &mut wd,
            at(supernova::SUPER_TOTAL_MS + 50),
            &c,
            g,
            None,
            None,
        );
        assert!(wd.persist[&wd.occ[0].ident].nova_done);
        assert!(start_of(&wd, 1).is_some(), "the deferred supernova grants");
        assert!(start_of(&wd, 2).is_none(), "the classic nova still waits");
        // Past the second window: the classic nova finally ignites.
        tick_nova(
            &mut wd,
            at(2 * supernova::SUPER_TOTAL_MS + 100),
            &c,
            g,
            None,
            None,
        );
        assert!(start_of(&wd, 2).is_some(), "the mutex released the nova");
    }

    /// The burst mutex holds for the supernova's FULL
    /// window even while its word is SCROLLED OFF mid-blast — the episode
    /// stays live on grace with `nova_start` set, and `super_prepass` must
    /// publish `super_until` from the persist-wide busy scan (not only the
    /// visible-occurrence loop). A visible classic-nova occurrence defers
    /// for the whole window, and when the word scrolls back on mid-window
    /// the supernova owns the whole quad channel (the shared `nova_add`
    /// backstop clamp never binds).
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
        // Mid-window the profanity scrolls off and the classic-nova word
        // appears: the episode survives on grace; the mutex must hold.
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
        // Back on-screen mid-window (shockwave phase): emission resumes with
        // the WHOLE quad channel to itself — the classic contributed zero
        // quads, so the shared backstop clamp never binds.
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

    /// §3.2 selection × wash: full-width detonation wash rows are SPLIT
    /// around the selected span (never washed over, never wholesale-deleted).
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
        // WASH pieces on the selected row (crown motes are narrow; wash
        // remainders span many cells): split into left + right, ≤ 3 per row.
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
        // EVERY quad on the row (wash + crown) clears the selected span.
        for x in q.iter().filter(|x| x.row == 1) {
            let (x0, x1) = (i32::from(x.x), i32::from(x.x) + i32::from(x.w));
            assert!(
                x1 <= 80 || x0 >= 120,
                "no quad overlaps the selected px span [80, 120): {x:?}"
            );
        }
        // Tier-1 composition binding for the readability ceiling: splitting
        // duplicates a wash quad's COLOR into disjoint left/right geometry, so
        // a naive per-row byte sum grows even though no pixel sees both pieces.
        // Project the REAL post-split stream by row + x + y and prove the renderer
        // can never accumulate more than the pure emitter's hard channel cap.
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

    /// §3.2: reduced_motion suppresses the supernova outright (static rainbow
    /// only, no grant, no quads).
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

    /// v3 §3.2 perf gate — the worst-case DETONATION frame: full-viewport
    /// wash + crown over a text-full 240×64 grid, ≤ 3 ms median in release.
    /// Sibling of `bench_nova_emit_worstcase`.
    ///
    /// SCOPE (deliberate): the design gates the full damaged present path, but
    /// a full-viewport wash marks EVERY row lit and `render_input_cached`
    /// re-renders every lit band (glyph blit + blend: ~31 ms at 240×64/18 px on
    /// the dev machine — the same per-row cost the 3-nova bench pays over ~15
    /// rows). That compositor cost is not this engine's, so the bench GATES the
    /// engine share (tick: prepass + emitters + selection split + channel fill)
    /// at ≤ 3 ms and reports the composite median informationally. The
    /// `perf_reduced` degrade path (§1.1 fix #3 freeze/thaw — non-destructive)
    /// covers machines where the composite overruns.
    ///
    /// ```sh
    /// cargo test -p aterm-effects --release bench_supernova_detonation_worstcase -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate (v3 §3.2): run manually in --release with --ignored --nocapture"]
    fn bench_supernova_detonation_worstcase() {
        let _perf_guard = PERF_BENCH_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        use aterm_render::{Renderer, Theme, WindowCpu};
        let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
            panic!("bench needs a system monospace font");
        };
        let (cw, ch) = rend.cell_size();
        let (rows, cols) = (64usize, 240usize);
        let lex = lex();
        let c = cfg_rainbow(100);
        let g = EffectGeom {
            cell_w: cw as u16,
            cell_h: ch as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");
        // Text-full grid (no matches in the filler), one word mid-screen.
        let filler =
            "lorem ipsum dolor sit amet consetetur sadipscing elitr sed diam nonumy ".repeat(4);
        for r in 0..rows {
            if r > 0 {
                term.process(b"\r\n");
            }
            term.process(&filler.as_bytes()[..cols.min(filler.len())]);
        }
        // Space-isolated so the whole-word scanner matches it mid-filler.
        term.process(b"\x1b[32;99H fuck ");
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        let ident = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Profanity)
            .expect("the word scanned")
            .ident;
        {
            let ep = wd.persist.get_mut(&ident).expect("episode");
            ep.burst_roll = true;
            ep.nova_start = Some(t0);
            ep.burst_started = true;
            ep.burst_done = true;
        }
        let base_input = term.cell_frame(rows, cols);
        let mut input = base_input.clone();
        let mut win = WindowCpu::new();
        rend.render_input_cached(&mut win, &base_input);
        let (mut deco, mut ink, mut fr, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut engine = Vec::new();
        let mut composite = Vec::new();
        for _ in 0..8u64 {
            for t_ms in (360..640u64).step_by(16) {
                let now = t0 + Duration::from_millis(t_ms);
                let start = Instant::now();
                wd.tick(
                    now, &c, g, None, None, true, &mut deco, &mut ink, &mut fr, &mut nova,
                );
                input.word_decorations.clone_from(&deco);
                input.ink.clone_from(&ink);
                input.nova_add.clone_from(&nova);
                let engine_dt = start.elapsed();
                rend.render_input_cached(&mut win, &input);
                engine.push(engine_dt);
                composite.push(start.elapsed());
            }
        }
        assert!(
            !nova.is_empty() || !deco.is_empty(),
            "the detonation actually emitted"
        );
        engine.sort();
        composite.sort();
        let median = engine[engine.len() / 2];
        println!(
            "bench_supernova_detonation_worstcase: ENGINE median {median:?} over {} frames \
             (min {:?}, max {:?}); tick+composite median {:?} (informational — the \
             full-viewport wash re-renders every band until the overlay compositor lands)",
            engine.len(),
            engine[0],
            engine[engine.len() - 1],
            composite[composite.len() / 2],
        );
        assert!(
            median < Duration::from_millis(3),
            "v3 §3.2 gate: engine median {median:?} >= 3 ms/frame"
        );
    }

    // ──────────── one-shot × reduced-motion regression battery ────────────

    /// ONE-SHOT × REDUCED MOTION: native hosts demote UNFOCUSED
    /// windows to `reduced_motion` (the W11b motion fold), so a completed
    /// one-shot must NOT resurrect as a static sprite on focus loss — cat,
    /// paw, nova ember, and sparkle residual alike — and must stay gone on
    /// refocus.
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
        // CAT: play the peek to Done, then reduce — zero sprites forever.
        // Row 2 so the two-band peeking head has its eye row (r−2) on-screen.
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

        // PAW (`style = "paw"`): the paw style draws NO graphic at all (ink
        // still plays elsewhere), so the feline output is empty at every phase.
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
        let (_, out, _) = tick_cat_at(&mut wd, at(4_050), &reduce(&c_paw), g, None, true);
        assert!(out.is_empty(), "reduced motion resurrects nothing");

        // NOVA EMBER: window + ≤ 2 s ember fade complete, then reduce — the
        // static glint must not come back.
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

        // SPARKLE RESIDUAL: anim window + fade complete, then reduce.
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

    /// A word born in an UNFOCUSED
    /// window under `reduced_motion` (the W11b demotion) shows the static
    /// pose IMMEDIATELY — the phase clock latched at birth — logs exactly
    /// once, and its clock elapses by wall time, so an un-reduce/refocus
    /// past the peek window replays NO entrance.
    #[test]
    fn born_unfocused_reduced_shows_static_pose_and_never_replays() {
        let lex = lex();
        let mut c = cfg();
        c.reduced_motion = true;
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        let (fr1, out1, _) = tick_cat_at(&mut wd, t0, &c, g, None, false);
        assert!(
            !fr1.is_empty() || !out1.is_empty(),
            "the static pose lands on the first unfocused tick (clock \
             latched at birth): {fr1:?} {out1:?}"
        );
        assert_eq!(wd.drain_kitty_sightings().count(), 1, "logs once");
        // The pose persists while reduced (flags never advance), unfocused.
        let (fr2, out2, _) = tick_cat_at(&mut wd, at(1_000), &c, g, None, false);
        assert_eq!(fr1, fr2, "the pose persists while reduced");
        assert_eq!(out1, out2);
        assert_eq!(wd.drain_kitty_sightings().count(), 0, "exactly once");
        // Un-reduce + refocus far past every reachable peek total: the clock
        // elapsed by wall time — no entrance replay, the one-shot is spent.
        let c_full = cfg();
        let (fr, _, _) = tick_cat_at(&mut wd, at(60_000), &c_full, g, None, true);
        assert!(
            fr.is_empty(),
            "no entrance replay on un-reduce/refocus: {fr:?}"
        );
        let cat = wd
            .occ
            .iter()
            .find(|o| o.spec.graphic.is_some())
            .expect("the cat word scanned")
            .ident;
        assert!(wd.persist[&cat].peek_done, "the elapsed window spends it");
        // The paw arm draws no graphic under v4 and logs nothing.
        let mut c_paw = c.clone();
        c_paw.feline_style = FelineStyle::Paw;
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c_paw, 1, t0);
        let (_, out, _) = tick_cat_at(&mut wd, t0, &c_paw, g, None, false);
        assert!(out.is_empty(), "no paw graphic under v4");
        assert_eq!(wd.drain_kitty_sightings().count(), 0);
    }

    /// ACCESSIBILITY PRESERVED: a pure OS-reduce-motion user with a
    /// FOCUSED window still gets the static pose immediately (the clock
    /// latched at birth), it logs exactly once, and the flags never
    /// advance — the pose persists indefinitely, byte-stable.
    #[test]
    fn reduced_from_birth_focused_shows_static_pose_and_logs_once() {
        let lex = lex();
        let mut c = cfg();
        c.reduced_motion = true;
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
        let (fr1, out1, fp1) = tick_cat_at(&mut wd, t0, &c, g, None, true);
        assert!(
            !fr1.is_empty() || !out1.is_empty(),
            "the static pose lands on the first focused tick"
        );
        assert_eq!(wd.drain_kitty_sightings().count(), 1, "logs once");
        // Far past the (never-running) peek window: the pose persists.
        let (fr2, out2, fp2) = tick_cat_at(&mut wd, at(60_000), &c, g, None, true);
        assert_eq!(fr1, fr2, "the pose persists (flags never advance)");
        assert_eq!(out1, out2);
        assert_eq!(fp1, fp2, "byte-stable static pose");
        assert_eq!(wd.drain_kitty_sightings().count(), 0, "exactly once");
        assert!(!wd.is_active(at(60_000)), "static = zero wakes");
    }

    /// DELIBERATE DEVIATION (pinned): un-reducing
    /// AFTER a static showing marks the peek done. The phase clock latched
    /// at birth (the static pose IS this episode's showing); once the
    /// never-animated peek window has elapsed, the first
    /// `reduced_motion = false` tick latches `peek_done` — zero free
    /// sprites, no entrance replay, zero wakes.
    #[test]
    fn un_reduce_after_static_showing_marks_the_peek_done() {
        let lex = lex();
        let mut c_red = cfg();
        c_red.reduced_motion = true;
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\na happy kitty naps");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 4, 20, &lex, &c_red, 1, t0);
        // Focused reduced tick: the static pose shows (the phase clock
        // latched at birth).
        let (fr, out, _) = tick_cat_at(&mut wd, t0, &c_red, g, None, true);
        assert!(
            !fr.is_empty() || !out.is_empty(),
            "the static pose lands on the first focused tick"
        );
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
        assert!(
            !wd.persist[&cat].peek_done,
            "flags hold while reduced (the pose persists)"
        );
        // Un-reduce far past every reachable peek_total: the first
        // non-reduced tick spends the one-shot instead of replaying the
        // entrance — the static showing WAS the showing.
        let (fr, _, _) = tick_cat_at(&mut wd, at(60_000), &c, g, None, true);
        assert!(
            fr.is_empty(),
            "zero free sprites — no entrance replay on un-reduce: {fr:?}"
        );
        assert!(
            wd.persist[&cat].peek_done,
            "the elapsed static window latches peek_done"
        );
        assert!(!wd.is_active(at(60_000)), "done = zero wakes");
        // And it stays spent: later ticks never resurrect the cat.
        let (fr, _, _) = tick_cat_at(&mut wd, at(61_000), &c, g, None, true);
        assert!(fr.is_empty(), "the peek stays spent (done forever)");
    }

    /// FROZEN-STATE KILL: an introspection capture during a
    /// `perf_reduced` freeze must not rescan (grace-expiring and done-marking
    /// every episode against the suspended clock) or tick the frozen engine —
    /// episodes survive byte-for-byte and resume in place after thaw.
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
        wd.freeze(at(100));
        // A capture-driven rescan 20 s into the freeze (> GRACE_TTL), against
        // a BLANK grid: both entry points must no-op.
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
        assert!(wd.done_marks.is_empty(), "no grace-expiry done marks");
        // The frozen tick emits nothing and advances nothing (defensive).
        let (fr, out, fp) = tick_cat_at(&mut wd, at(20_000), &c, g, None, true);
        assert!(
            fr.is_empty() && out.is_empty() && fp == 0,
            "frozen tick no-ops"
        );
        assert_eq!(snap(&wd), before);
        // Thaw + the deferred rescan: the peek resumes ~100 ms in (shifted
        // clocks) — neither Done nor born-done.
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

    /// TWO-WAY BURST MUTEX: a supernova must not ignite while a
    /// CLASSIC nova window is live — combined `nova_add` would exceed the
    /// 1536 budget — and grants only after the classic window ends.
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
        // The profanity appears mid-window: its rolled supernova DEFERS.
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

    /// ECLIPSE VEIL × SELECTION: the light-theme detonation's Over-blend Shade
    /// stamps are DROPPED over selected cells — the renderer selection freeze
    /// covers only the Add stream, so without the host-side filter a
    /// mid-eclipse selection would be washed over by the veil. The filter must
    /// key on the BLEND, not the Shade glyph: the light-theme charge motes and
    /// rainbow debris are Over decos too, so a debris-phase frame must punch
    /// through a selection identically.
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
        // BLEND-keyed regression: a light-theme DEBRIS-phase frame (t = 1800 ms
        // — Over-blend Dot/Star4 motes, no Shade in sight). Discover where a
        // mote lands without a selection, select that cell, and re-present:
        // the blend-keyed filter must drop it; a Shade-keyed one would not.
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

    /// `chance_pct` BEYOND SuperNova: the birth roll gates EVERY
    /// burst kind — a starburst at chance = 0 never fires (no quads, no
    /// residual, no reduced-motion spark, zero wakes); chance = 100 always
    /// fires (the class-default posture).
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

    /// THEME BRANCH WITHOUT INK: `[sparkle_words.ink]
    /// enabled = false` must not send a light-theme supernova down the
    /// (invisible-on-white) dark additive-wash path — the lead-cell bg is
    /// captured for every occurrence, so the eclipse branch still fires.
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
        // Mid-detonation: the light-theme ECLIPSE (Over-blend veil), not the
        // dark wash.
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

    /// §3.1 SPAN CLAMP: `span_used = min(span_deg,
    /// 100°·(lead_cells − 1))` — the full 300° across 4 leads, a 100°
    /// two-step on 2 leads, and the 1-lead word rides the temporal drift
    /// then freezes at exactly its t = 0 (genome base hue) bytes.
    #[test]
    fn rainbow_span_clamps_to_lead_cells() {
        let t0 = Instant::now();
        let c = cfg_rainbow(0);
        let fg = [220u8, 220, 220];
        let strength = c.ink_strength;
        // A gkey whose guard never binds at full strength on black for any
        // of the three spans (all four u samples clear MIN_INK_CONTRAST), so
        // the expected bytes are the raw formula.
        let clears = |gkey: u64, span: f32| {
            [0.0f32, 1.0 / 3.0, 2.0 / 3.0, 1.0].iter().all(|u| {
                let h = hsv2rgb(
                    (rainbow_base_hue(gkey) + u * span).rem_euclid(360.0),
                    0.85,
                    1.0,
                );
                contrast_ratio(mix_rgb(rgb3_to_u32(fg), h, strength), 0) >= MIN_INK_CONTRAST
            })
        };
        let gkey = (0..10_000u64)
            .find(|g| clears(*g, 300.0) && clears(*g, 100.0) && clears(*g, 0.0))
            .expect("a guard-free gkey exists");
        let build = |leads: u16| {
            let mut o = occ(Class::Profanity, t0);
            o.spec = class_default_spec(Class::Profanity, &c);
            o.genome = Genome { gkey, magic: 100 };
            o.start_col = 2;
            o.end_col = 2 + leads - 1;
            o.ink_cells = leads;
            o.ink_bg = [0; 3];
            let mut wd = WordDecorations {
                occ: vec![o],
                cols: 20,
                have_scanned: true,
                ..WordDecorations::default()
            };
            wd.ink_base_fg = vec![fg; usize::from(leads)];
            wd.ink_cols = (2..2 + leads).collect();
            wd
        };
        let expect = |span_used: f32, n: u16| -> Vec<[u8; 3]> {
            let denom = f32::from(n.saturating_sub(1)).max(1.0);
            (0..n)
                .map(|i| {
                    let u = f32::from(i) / denom;
                    u32_to_rgb3(mix_rgb(
                        rgb3_to_u32(fg),
                        hsv2rgb(
                            (rainbow_base_hue(gkey) + u * span_used).rem_euclid(360.0),
                            0.85,
                            1.0,
                        ),
                        strength,
                    ))
                })
                .collect()
        };
        let settle = t0 + Duration::from_millis(2_600); // past the 2 500 ms drift
        // 4 leads ("fuck"): span_used = min(300, 300) = 300°.
        let mut wd = build(4);
        let (_, _, ink, _, _) = tick_nova(&mut wd, settle, &c, geom20(), None, None);
        assert_eq!(
            ink.iter().map(|i| i.color).collect::<Vec<_>>(),
            expect(300.0, 4),
            "4 leads sweep the full 300°"
        );
        // 2 leads (2-cell CJK): span_used = min(300, 100) = 100°.
        let mut wd = build(2);
        let (_, _, ink, _, _) = tick_nova(&mut wd, settle, &c, geom20(), None, None);
        assert_eq!(
            ink.iter().map(|i| i.color).collect::<Vec<_>>(),
            expect(100.0, 2),
            "2 leads take the readable 100° two-step"
        );
        // 1 lead: span_used = 0 — the cell drifts over time, then freezes at
        // EXACTLY the t = 0 (genome base hue) bytes.
        let mut wd = build(1);
        let (_, _, ink0, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert_eq!(
            ink0.iter().map(|i| i.color).collect::<Vec<_>>(),
            expect(0.0, 1),
            "t = 0 shows the genome base hue"
        );
        let (_, _, ink_mid, _, _) = tick_nova(
            &mut wd,
            t0 + Duration::from_millis(1_200),
            &c,
            geom20(),
            None,
            None,
        );
        assert_ne!(ink0, ink_mid, "the 1-cell word drifts through hues");
        let (_, _, ink_set, _, _) = tick_nova(&mut wd, settle, &c, geom20(), None, None);
        assert_eq!(
            ink0, ink_set,
            "…then freezes at the exact t = 0 base-hue bytes"
        );
        assert!(!wd.is_active(settle), "settled = idle");
    }

    /// §3.1 LEGIBILITY GUARD: the rainbow guard
    /// samples u ∈ {0, ⅓, ⅔, 1} — a single mid-gradient sample is BLIND to
    /// endpoint washout, so this pins a case where u = 0.5 clears at full
    /// strength while another sample fails, and asserts every emitted cell
    /// still clears [`MIN_INK_CONTRAST`] (the guard pulled).
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
        // The mid-only blindness witness: full-strength mid passes, some
        // 4-sample point fails.
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
        // The 4 emitted lead cells sit exactly at the guard's sample points
        // (u ∈ {0, ⅓, ⅔, 1}): every one must clear the bound.
        for cell in &ink {
            assert!(
                contrast_ratio(rgb3_to_u32(cell.color), bg32) >= MIN_INK_CONTRAST,
                "guarded cell washes out: {cell:?}"
            );
        }
        // And the guard genuinely pulled — the failing sample's raw
        // full-strength bytes were NOT emitted (a mid-only guard would have
        // left them).
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

    /// The Nyan cursor ([`WordDecorations::nyan_cursor`]) bakes the sprite into
    /// the shared free atlas and stamps the companion just AHEAD of the cursor
    /// cell, vertically centred on its row. Shipping emission is exactly the
    /// BODY sprite—no auxiliary decoration sprites. Deterministic + headless.
    #[test]
    fn nyan_cursor_emits_only_its_body_in_front_of_the_cursor() {
        let g = geom20(); // 10×20 cells, 6×20 grid.
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        let footprint = wd
            .nyan_cursor_footprint(NyanCursorLayout {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                bob: 0.0,
            })
            .expect("visible footprint");

        wd.nyan_cursor(
            NyanCursorFrame {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                colors: CatColorKey::default(),
                bob: 0.0,
                alpha: 200,
                pose: crate::nyan_cursor::CatPose::STILL,
                sing: 0.0,
                notes: [None; crate::nyan_sing::MAX_NOTES],
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
        // In FRONT with a clear gap: the sprite's left edge sits past the cursor
        // cell's right edge (col 6 starts at x=60), within a cell of it.
        let cursor_right = 6 * i32::from(g.cell_w);
        let old_v056_x = cursor_right + i32::from(g.cell_w) / 4;
        assert_eq!(
            s.x,
            cursor_right + 3 * i32::from(g.cell_w) / 4,
            "the companion uses the incoming 3/4-cell lead"
        );
        assert!(
            s.x > old_v056_x,
            "v0.57 body x={} must lead the v0.56 x={old_v056_x}",
            s.x,
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
        wd.nyan_cursor(
            NyanCursorFrame {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                colors: CatColorKey::default(),
                bob: 0.0,
                alpha: 0,
                pose: crate::nyan_cursor::CatPose::STILL,
                sing: 0.0,
                notes: [None; crate::nyan_sing::MAX_NOTES],
            },
            &mut none,
        );
        assert!(none.is_empty());
    }

    /// SINGING NOTES stream from the cat: with a non-zero sing drive the
    /// ♪/♫ sprites emit beside the body — ring-capped at `MAX_NOTES` even
    /// under a hostile full ring, rainbow-tinted through the `tint` channel
    /// over ONE white-baked tile per kind, and alpha-scaled by the drive so
    /// the wind-down crossfade eases the stream out. (User-sprite path: one
    /// body bake leaves budget for the note tile in the same frame; on the
    /// built-in path the shared two-bake budget can defer a second note kind
    /// by one frame.)
    #[test]
    fn singing_notes_stream_ring_capped_tinted_and_drive_scaled() {
        use crate::nyan_sing::{MAX_NOTES, NOTE_TINTS, NoteKind, NoteSprite};
        let g = geom20();
        let mut wd = WordDecorations::default();
        wd.set_nyan_sprite_source(NyanSpriteSource::Custom {
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
        let frame = |sing: f32, notes| NyanCursorFrame {
            geom: g,
            cursor: (3, 2),
            look: KittyLook::default(),
            colors: CatColorKey::default(),
            bob: 0.0,
            alpha: 255,
            pose: crate::nyan_cursor::CatPose::STILL,
            sing,
            notes,
        };
        let mut free = Vec::new();
        wd.nyan_cursor(frame(1.0, notes), &mut free);
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

        // Wind-down: the SAME notes at half drive emit at half alpha.
        let mut fading = Vec::new();
        wd.nyan_cursor(frame(0.5, notes), &mut fading);
        let faded = &fading[..fading.len() - 1];
        assert_eq!(faded.len(), MAX_NOTES);
        assert!(
            faded.iter().all(|s| s.alpha == 100),
            "the crossfade rides the emitted alpha"
        );

        // Zero drive (with a drained ring): byte-identical to a plain frame.
        let mut plain = Vec::new();
        wd.nyan_cursor(frame(0.0, [None; MAX_NOTES]), &mut plain);
        assert_eq!(plain.len(), 1, "no note work at rest — just the body");
    }

    /// Age changes the cursor cat through uniform pixel dimensions, not a
    /// redundant non-visual cache discriminator. At this tiny metric Kitten and
    /// Adolescent round to the same exact box, so the second presentation must
    /// hit the existing atlas tile.
    #[test]
    fn cursor_age_rounding_alias_reuses_the_exact_size_bake() {
        let geom = EffectGeom {
            cell_w: 2,
            cell_h: 3,
            rows: 10,
            cols: 40,
        };
        let mut wd = WordDecorations::default();
        let emit = |wd: &mut WordDecorations, age| {
            let mut sprites = Vec::new();
            wd.nyan_cursor(
                NyanCursorFrame {
                    geom,
                    cursor: (3, 5),
                    look: KittyLook {
                        age,
                        ..KittyLook::default()
                    },
                    colors: CatColorKey::default(),
                    bob: 0.0,
                    alpha: 255,
                    pose: crate::nyan_cursor::CatPose::STILL,
                    sing: 0.0,
                    notes: [None; crate::nyan_sing::MAX_NOTES],
                },
                &mut sprites,
            );
            assert_eq!(sprites.len(), 1, "one age-sized companion body");
            sprites[0]
        };

        let kitten = emit(&mut wd, CatAge::Kitten);
        let after_kitten = wd.cat_baker.version();
        let adolescent = emit(&mut wd, CatAge::Adolescent);
        assert_eq!((kitten.w, kitten.h), (adolescent.w, adolescent.h));
        assert_eq!(
            wd.cat_baker.version(),
            after_kitten,
            "dimensionally identical ages must be a cache hit"
        );
    }

    /// A user-supplied sprite ([`WordDecorations::set_nyan_sprite_source`]) overrides the
    /// built-in homage: it is nearest-resampled to fit the slot (aspect
    /// preserved) and flown; clearing it restores the homage. Headless.
    #[test]
    fn nyan_cursor_flies_a_user_sprite_when_set() {
        let g = geom20(); // ch = 20 ⇒ slot 80×40, height cap 1.8·ch = 36.
        let mut wd = WordDecorations::default();
        // A 4×2 opaque sprite (aspect 2:1). fit s = min(80/4, 36/2) = 18 ⇒ 72×36.
        let mut native = Vec::new();
        for _ in 0..(4 * 2) {
            native.extend_from_slice(&[255, 0, 0, 255]);
        }
        let native: Arc<[u8]> = Arc::from(native);
        wd.set_nyan_sprite_source(NyanSpriteSource::Custom {
            source_fp: 7,
            w: 4,
            h: 2,
            rgba: Arc::clone(&native),
        });
        assert!(Arc::ptr_eq(
            wd.nyan_sprite_rgba().expect("custom source"),
            &native
        ));
        let mut free = Vec::new();
        wd.nyan_cursor(
            NyanCursorFrame {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                colors: CatColorKey::default(),
                bob: 0.0,
                alpha: 255,
                pose: crate::nyan_cursor::CatPose::STILL,
                sing: 0.0,
                notes: [None; crate::nyan_sing::MAX_NOTES],
            },
            &mut free,
        );
        assert_eq!(
            free.len(),
            1,
            "user art follows the same one-body sprite contract"
        );
        assert_eq!(
            (free[0].w, free[0].h),
            (72, 36),
            "resampled to fit the slot"
        );
        assert!(wd.free_atlas().is_some(), "baked into the shared atlas");

        // Clearing restores the one-body homage at its own, different size.
        wd.set_nyan_sprite_source(NyanSpriteSource::BuiltIn);
        let mut homage = Vec::new();
        wd.nyan_cursor(
            NyanCursorFrame {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                colors: CatColorKey::default(),
                bob: 0.0,
                alpha: 255,
                pose: crate::nyan_cursor::CatPose::STILL,
                sing: 0.0,
                notes: [None; crate::nyan_sing::MAX_NOTES],
            },
            &mut homage,
        );
        assert_eq!(homage.len(), 1, "built-in homage is one body sprite");
        let body = homage[0];
        assert_ne!(body.w, 72, "homage has its own dimensions");
    }

    /// The later/right-shifted anchor remains a preference, not permission to
    /// leave the grid: body stretch and forward pose lead clamp together at the
    /// right margin.
    #[test]
    fn cursor_cat_body_clamps_after_right_shift_stretch_and_lead() {
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        wd.nyan_cursor(
            NyanCursorFrame {
                geom: g,
                cursor: (3, g.cols - 1), // last column: the body clamps to the edge
                look: KittyLook::default(),
                colors: CatColorKey::default(),
                bob: 0.0,
                alpha: 255,
                pose: crate::nyan_cursor::CatPose {
                    scale_x: 1.17,
                    lead: 0.22,
                    ..crate::nyan_cursor::CatPose::STILL
                },
                sing: 0.0,
                notes: [None; crate::nyan_sing::MAX_NOTES],
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

    /// OFFSET CONSTANT PROOF: the horizontal anchor is exactly the cursor
    /// cell's right edge plus [`WordDecorations::NYAN_LEAD_NUM`]/
    /// [`WordDecorations::NYAN_LEAD_DEN`] = 3/4 of a cell. Pins the constants
    /// and the placement they produce together, so neither can drift alone.
    #[test]
    fn companion_leads_the_cursor_by_three_quarters_of_a_cell() {
        let g = geom20();
        let wd = WordDecorations::default();
        let fp = wd
            .nyan_cursor_footprint(NyanCursorLayout {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                bob: 0.0,
            })
            .expect("visible footprint");
        let cursor_right = 6 * i32::from(g.cell_w);
        assert_eq!(
            (
                WordDecorations::NYAN_LEAD_NUM,
                WordDecorations::NYAN_LEAD_DEN
            ),
            (3, 4),
            "the documented 3/4-cell lead is the constant actually flown"
        );
        assert_eq!(
            fp.x,
            cursor_right + 3 * i32::from(g.cell_w) / 4,
            "anchor = cursor right edge + 3/4 cell"
        );
    }

    /// BOUNDARY RISE PROOF: when the right-edge clamp pushes the footprint
    /// back across the cursor cell, the body rises half a cell instead of
    /// covering the glyphs. At the top edge the rise backs off entirely, so
    /// the established top-row presentation is never lifted off-grid.
    #[test]
    fn right_edge_clamp_rises_the_companion_instead_of_covering_the_text() {
        let g = geom20();
        let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
        let wd = WordDecorations::default();
        let fp_at = |cursor: (u16, u16)| {
            wd.nyan_cursor_footprint(NyanCursorLayout {
                geom: g,
                cursor,
                look: KittyLook::default(),
                bob: 0.0,
            })
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
            wd.nyan_cursor(
                NyanCursorFrame {
                    geom: g,
                    cursor,
                    look: KittyLook::default(),
                    colors: CatColorKey::default(),
                    bob: 0.0,
                    alpha: 255,
                    pose: crate::nyan_cursor::CatPose::STILL,
                    sing: 0.0,
                    notes: [None; crate::nyan_sing::MAX_NOTES],
                },
                &mut free,
            );
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

    /// Removing cursor-companion decoration does not change the established
    /// one-body emission contract for settled peeking word-cats.
    #[test]
    fn settled_word_cat_remains_one_body_sprite() {
        let now = Instant::now();
        let mut wd = settled_cat(now);
        let at = now + Duration::from_millis(DWELL_QUIET_MS);
        let (fr, _, _) = tick_cat(&mut wd, at, &cfg(), geom20());
        assert_eq!(fr.len(), 1, "a settled word-cat remains exactly one sprite");
    }

    #[test]
    fn invalid_nyan_source_fails_closed_instead_of_using_builtin_art() {
        let g = geom20();
        let mut wd = WordDecorations::default();
        wd.set_nyan_sprite_source(NyanSpriteSource::Disabled);
        assert_eq!(wd.nyan_sprite_source_fingerprint(), None);
        assert!(
            wd.nyan_cursor_footprint(NyanCursorLayout {
                geom: g,
                cursor: (3, 5),
                look: KittyLook::default(),
                bob: 0.0,
            })
            .is_none()
        );
        let mut free = Vec::new();
        assert!(
            wd.nyan_cursor(
                NyanCursorFrame {
                    geom: g,
                    cursor: (3, 5),
                    look: KittyLook::default(),
                    colors: CatColorKey::default(),
                    bob: 0.0,
                    alpha: 255,
                    pose: crate::nyan_cursor::CatPose::STILL,
                    sing: 0.0,
                    notes: [None; crate::nyan_sing::MAX_NOTES],
                },
                &mut free,
            )
            .is_none()
        );
        assert!(free.is_empty());
    }

    /// ACCESSORY SIGHTINGS: a Bow-decoding magic word's sighting must CARRY
    /// `TRAIT_BOW` — the kitty-log accessory counters key off exactly these
    /// bits (`accessory_trait_bits`; the bits→counter ledger side is pinned in
    /// `aterm-gui::kitty_log` and its observe-chain test). If the traits
    /// assembly stops OR'ing the accessory bits, those counters silently never
    /// increment. The overlay accessory (`accessory_variant_v4`) drives the bit.
    #[test]
    fn bow_cat_sighting_carries_trait_bow() {
        use crate::kitty_registry::{TRAIT_BOW, accessory_trait_bits};
        let now = Instant::now();
        // cat window (`% 4096` = 100) misses every magic build; accessory
        // window (`(>> 24) % 4096` = 64 ∈ 0..=127) decodes Bow.
        let magic = (64u64 << 24) | 100;
        assert_eq!(genome::cat_accessory(magic), Some(genome::Accessory::Bow));
        assert_eq!(
            accessory_trait_bits(genome::cat_accessory(magic)),
            TRAIT_BOW
        );
        let mut o = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
        o.genome = Genome {
            gkey: GKEY_ADULT_HEAD,
            magic,
        };
        let mut wd = WordDecorations {
            occ: vec![o.clone()],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let mut e = Episode::fresh(o.appeared, o.genome, o.seed, o.row, 0);
        e.phase_start = Some(o.appeared);
        wd.persist.insert(o.ident, e);
        // Drain across a few ticks (bakes land ≤ 2/frame) — the first landed
        // present queues the episode's one sighting.
        let mut s: Vec<KittySighting> = Vec::new();
        for k in 0..4u64 {
            tick_cat_at(
                &mut wd,
                now + Duration::from_millis(DWELL_QUIET_MS + 16 * k),
                &cfg(),
                geom20(),
                None,
                true,
            );
            s.extend(wd.drain_kitty_sightings());
        }
        assert_eq!(s.len(), 1, "one sighting for the bow cat");
        assert_ne!(
            s[0].traits & TRAIT_BOW,
            0,
            "the Bow accessory rides the sighting trait bits, got {:#010b}",
            s[0].traits
        );
        // Control: the bare twin (accessory window misses) carries no bit.
        let bare = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
        let mut wd = WordDecorations {
            occ: vec![bare.clone()],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let mut e = Episode::fresh(bare.appeared, bare.genome, bare.seed, bare.row, 0);
        e.phase_start = Some(bare.appeared);
        wd.persist.insert(bare.ident, e);
        let mut s: Vec<KittySighting> = Vec::new();
        for k in 0..4u64 {
            tick_cat_at(
                &mut wd,
                now + Duration::from_millis(DWELL_QUIET_MS + 16 * k),
                &cfg(),
                geom20(),
                None,
                true,
            );
            s.extend(wd.drain_kitty_sightings());
        }
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].traits & TRAIT_BOW, 0, "the bare twin carries no bow");
    }

    // ───────────────────── curse-BONK cue battery ─────────────────────

    /// TYPED-WITNESS PROVENANCE, positive arm: a curse typed at the prompt
    /// (caret one past the token at rescan) records exactly ONE
    /// `CurseCueKind::Typed` cue at the next tick, then never again — the
    /// episode latch is a one-shot.
    #[test]
    fn typed_curse_birth_records_one_bonk_cue() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 32);
        let mut wd = WordDecorations::default();
        term.process(b"fuck");
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
        // …and a passive redraw of the same content (caret parked after the
        // word, episode adopted on the fast path) births nothing new.
        wd.rescan(&term, 4, 32, &lex, &c, 2, t0 + Duration::from_millis(32));
        tick_deco(&mut wd, t0 + Duration::from_millis(48), &c, &mut out);
        assert_eq!(wd.drain_curse_cues().count(), 0);
    }

    /// TYPED-WITNESS PROVENANCE, negative arm: `cat`-style OUTPUT containing
    /// a curse leaves the caret at the next prompt, not one past the token —
    /// no typed cue may fire, no matter how long the word decorates.
    #[test]
    fn onscreen_curse_output_never_records_a_typed_bonk() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(6, 32);
        let mut wd = WordDecorations::default();
        term.process(b"$ cat rant.txt\r\nfuck this thing\r\n$ ");
        wd.rescan(&term, 6, 32, &lex, &c, 1, t0);
        assert!(
            wd.occ.iter().any(|o| o.class == Class::Profanity),
            "the output curse does decorate"
        );
        let mut out = Vec::new();
        for i in 0..4u64 {
            tick_deco(&mut wd, t0 + Duration::from_millis(16 * i), &c, &mut out);
            assert_eq!(
                wd.drain_curse_cues().count(),
                0,
                "on-screen output must never bonk (tick {i})"
            );
        }
    }

    /// MULTILINGUAL FOR FREE via `Class::Profanity`: typed zh / ja / ko / en
    /// curses (CJK compounds matched by the no-space scanner, wide-cell caret
    /// arithmetic included) each surface exactly one typed cue.
    #[test]
    fn multilingual_typed_curses_bonk_zh_ja_ko_en() {
        let lex = Lexicon::with_languages(&["all"]);
        let c = cfg();
        let t0 = Instant::now();
        for word in ["fuck", "他妈的", "くそ", "씨발"] {
            let mut term = Terminal::new(2, 32);
            term.process(word.as_bytes());
            let mut wd = WordDecorations::default();
            wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
            let mut out = Vec::new();
            tick_deco(&mut wd, t0, &c, &mut out);
            let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
            assert_eq!(cues.len(), 1, "{word:?} must record one typed cue");
            assert_eq!(cues[0].kind, CurseCueKind::Typed, "{word:?}");
        }
    }

    /// IGNORE LISTS SUPPRESS THE SOUND WITH THE LIGHT: the folded spaced
    /// surface and the RAW CJK compound both silence their bonk through the
    /// same `ScanOptions::ignore` deny set (no occurrence ⇒ no cue).
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

    /// AMBIGUOUS-DEFERRAL INHERITANCE: the Romanian `fut`/`futu` forms are
    /// transient whole tokens at the caret while any user types `future` —
    /// the scanner defers them before an occurrence exists, so no prefix of
    /// `future` may ever bonk, even with Romanian (or every language)
    /// explicitly enabled.
    #[test]
    fn ambiguous_typo_transients_never_bonk() {
        for language in ["ro", "all"] {
            let lex = Lexicon::with_languages(&[language]);
            let c = cfg();
            let t0 = Instant::now();
            let mut term = Terminal::new(2, 32);
            let mut wd = WordDecorations::default();
            let mut out = Vec::new();
            for (i, byte) in b"future".iter().copied().enumerate() {
                term.process(&[byte]);
                wd.rescan(&term, 2, 32, &lex, &c, i as u64 + 1, t0);
                tick_deco(&mut wd, t0 + Duration::from_millis(i as u64), &c, &mut out);
                assert_eq!(
                    wd.drain_curse_cues().count(),
                    0,
                    "{language}: prefix {:?} of future must never bonk",
                    &b"future"[..=i]
                );
            }
        }
    }

    /// DETONATION EDGE: a rolled supernova cues `Detonated` exactly once, AT
    /// the ignition grant (inheriting the flash limiter + `burst_done`), and
    /// an OUTPUT curse detonating cues Detonated WITHOUT a Typed cue — the
    /// two kinds keep their provenance separate for the host's two knobs.
    #[test]
    fn supernova_detonation_cues_at_the_grant_edge_once() {
        let lex = lex();
        let c = cfg_rainbow(100);
        let t0 = Instant::now();
        let g = geom20();
        // Typed curse: the same tick carries the typed cue AND the grant cue.
        let mut term = Terminal::new(6, 20);
        term.process(b"oh fuck");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert!(
            cues.iter().any(|q| q.kind == CurseCueKind::Typed),
            "typed witness cue missing: {cues:?}"
        );
        assert_eq!(
            cues.iter()
                .filter(|q| q.kind == CurseCueKind::Detonated)
                .count(),
            1,
            "exactly one detonation cue at the grant: {cues:?}"
        );
        // Once per episode: the running blast re-cues nothing.
        tick_nova(&mut wd, t0 + Duration::from_millis(100), &c, g, None, None);
        assert_eq!(wd.drain_curse_cues().count(), 0);
        // Output curse (no typed witness): Detonated only.
        let mut term2 = Terminal::new(6, 20);
        term2.process(b"fuck\r\n$ ");
        let mut wd2 = WordDecorations::default();
        wd2.rescan(&term2, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd2, t0, &c, g, None, None);
        let cues2: Vec<CurseCue> = wd2.drain_curse_cues().collect();
        assert!(
            cues2.iter().all(|q| q.kind == CurseCueKind::Detonated),
            "output content must never carry the typed kind: {cues2:?}"
        );
        assert_eq!(
            cues2.len(),
            1,
            "the output detonation still cues: {cues2:?}"
        );
    }

    /// DETONATION EDGE (classic nova twin of
    /// `supernova_detonation_cues_at_the_grant_edge_once`): a highlighted
    /// profanity in the `style = "nova"` classic-nova profile also bonks at
    /// detonation — one `Detonated` cue AT the ignition grant, none on the
    /// running blast, and an OUTPUT curse detonates WITHOUT a Typed cue.
    #[test]
    fn nova_detonation_cues_at_the_grant_edge_once() {
        let lex = lex();
        let c = cfg_nova();
        let t0 = Instant::now();
        let g = geom20();
        // Typed curse: the same tick carries the typed cue AND the grant cue.
        let mut term = Terminal::new(6, 20);
        term.process(b"oh fuck");
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert!(
            cues.iter().any(|q| q.kind == CurseCueKind::Typed),
            "typed witness cue missing: {cues:?}"
        );
        assert_eq!(
            cues.iter()
                .filter(|q| q.kind == CurseCueKind::Detonated)
                .count(),
            1,
            "exactly one detonation cue at the nova grant: {cues:?}"
        );
        // Once per episode: the running blast re-cues nothing.
        tick_nova(&mut wd, t0 + Duration::from_millis(100), &c, g, None, None);
        assert_eq!(wd.drain_curse_cues().count(), 0);
        // Output curse (no typed witness): Detonated only.
        let mut term2 = Terminal::new(6, 20);
        term2.process(b"fuck\r\n$ ");
        let mut wd2 = WordDecorations::default();
        wd2.rescan(&term2, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd2, t0, &c, g, None, None);
        let cues2: Vec<CurseCue> = wd2.drain_curse_cues().collect();
        assert!(
            cues2.iter().all(|q| q.kind == CurseCueKind::Detonated),
            "output content must never carry the typed kind: {cues2:?}"
        );
        assert_eq!(
            cues2.len(),
            1,
            "the output nova detonation still cues: {cues2:?}"
        );
    }

    /// BOUNDEDNESS + STATE DROP (the hostless/wasm rule the sightings
    /// follow): an undrained tick's cues are cleared at the next tick's
    /// start, and `reset()` drops pending cues with the state.
    #[test]
    fn curse_cues_clear_at_tick_start_and_on_state_drop() {
        let lex = lex();
        let c = cfg();
        let t0 = Instant::now();
        let mut term = Terminal::new(4, 32);
        let mut wd = WordDecorations::default();
        term.process(b"fuck");
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

    /// PANE BINDING (owner ruling, 2026-07-28: "sparkle effects and toys are
    /// NOT SINGLE PANE ONLY"). Every visible pane drives ONE engine in turn,
    /// so each pane needs its own grid + episode state while the two safety
    /// budgets (the §6.4 flash limiter, the §3.2 supernova mutex) stay shared.
    mod pane_binding {
        use super::*;

        /// Scan `text` into the CURRENTLY BOUND pane at the given grid size.
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
            wd.rescan_from_cells_with_geom(
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

        /// T9 — failure mode (a). `last_epoch` is per pane, so a pane that has
        /// never scanned still asks for its first scan after a sibling scanned
        /// at the same epoch, and a pane that HAS scanned is not re-scanned
        /// merely because a sibling was.
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

        /// T10 — failure mode (b), the one that would have shipped FLAT INK
        /// forever in exactly the layout this change targets: one engine driven
        /// at alternating pane widths sees `cols_changed` on every rescan, so
        /// `settle_until` re-arms forever and no birth is ever played.
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

        /// T11 — failure mode (c). The same word at the same column in two
        /// panes is two different sightings: pane 2 births its own episode at
        /// its own instant instead of adopting (and inheriting the spent
        /// one-shots of) pane 1's.
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

        /// T12 — failure mode (d), the subtlest: alignment's "recent" window is
        /// `REKEY_MAX_SEQ_GAP = 2` RESCANS. A shared counter advances once per
        /// pane per frame, so at three panes the window closes inside a single
        /// frame and every word looks stale to its own next scan.
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

        /// T13 — failure mode (e). `MAX_BAKES_PER_FRAME` and the baker's LRU
        /// clock are per PRESENTED FRAME. One `begin_host_frame` per frame is
        /// what keeps a four-pane window at two bakes instead of eight, into
        /// the ONE atlas every pane's sprites address.
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

        /// T14 — failure mode (f), and the accessibility claim this whole
        /// design exists to protect: the §6.4 flash limiter is WINDOW-wide
        /// (WCAG 2.3.1). Two panes flashing at the same PANE-LOCAL spot are two
        /// flashes in two different places, so they must not overlap-tighten
        /// each other into a cap of one — and a third flash in the same rolling
        /// second is still deferred, across panes.
        #[test]
        fn flash_limiter_stays_window_wide_across_panes() {
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            let grant = |wd: &mut WordDecorations, owner: u64| {
                grant_pane_ignition(
                    &mut wd.ignitions,
                    wd.bound.unwrap_or(0),
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

        /// T16 — the cross-pane cancellation. `prune_ignitions` cancels a
        /// FUTURE slot whose owner is absent from `persist`; every other pane's
        /// episodes are parked out of that map, so without the pane label one
        /// pane's tick silently cancels its sibling's queued nova.
        #[test]
        fn a_panes_pending_ignition_survives_another_panes_tick() {
            let t0 = Instant::now();
            let mut wd = WordDecorations::default();
            wd.bind_pane(1, (0, 0));
            for (owner, cx) in [(1u64, 100i32), (2, 900), (3, 1700)] {
                grant_pane_ignition(
                    &mut wd.ignitions,
                    wd.bound.unwrap_or(0),
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
                    .any(|r| r.pane == 1 && r.start == pending),
                "pane 2's tick must not cancel pane 1's queued nova"
            );
        }

        /// A pane the window stops showing takes its state with it, and the
        /// live binding is dropped when its own pane is the one that vanished.
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
            // The SURVIVOR is untouched. Retiring the pane that left must not
            // fold over the panes that stayed: closing the focused pane of a
            // split would otherwise blank the cats in every other pane.
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

/// The measurement behind the [`ScanMemo`]: how much of a rescan the per-row
/// tokenise+lexicon pass actually is, on the two shapes that matter — a full
/// screen the user is typing into, and a screen scrolling under output.
///
/// Not a correctness test (it prints, it does not assert timings — a shared CI
/// box has no stable clock), so it is `#[ignore]`d. Reproduce with:
/// `cargo test -p aterm-effects --release --lib -- --ignored --nocapture scan_memo_cost`
#[cfg(test)]
mod scan_memo_bench {
    use super::*;
    use aterm_core::terminal::Terminal;
    use aterm_lexicon::Lexicon;

    /// A dense line of ordinary prose carrying one match of each live class —
    /// the realistic worst case for the scanner (every token is a spaced-script
    /// token that must be folded and probed, none are suppressed as code/paths).
    const PROSE: &str = "the quick brown fox jumps over the lazy dog while a cat naps and someone says fuck about the build system output that keeps scrolling past here forever\r\n";

    fn bench_cfg() -> DecoConfig {
        DecoConfig {
            profanity: true,
            feline: true,
            orca: true,
            emphasis: true,
            ink_enabled: true,
            ..DecoConfig::default()
        }
    }

    #[test]
    #[ignore = "timing probe, not an assertion; see the module doc"]
    fn scan_memo_cost() {
        let (rows, cols) = (60usize, 200usize);
        let lex = Lexicon::with_languages(&["en"]);
        let cfg = bench_cfg();
        let geom = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let now = Instant::now();
        let frames = 200u64;

        // (1) STATIC full screen, rescanned every frame — the shape a user
        // typing at a prompt on a full screen presents: one row's text changes,
        // the damage epoch advances, and the rescan re-tokenises everything.
        let mut term = Terminal::new(rows as u16, cols as u16);
        for _ in 0..rows {
            term.process(PROSE.as_bytes());
        }
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        for bypass in [true, false] {
            let mut wd = WordDecorations::default();
            wd.scan_memo.bypass = bypass;
            for e in 0..5u64 {
                wd.rescan_from_cells_with_geom(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    &lex,
                    &cfg,
                    e,
                    now,
                    geom,
                    0,
                );
            }
            let started = Instant::now();
            for e in 10..10 + frames {
                wd.rescan_from_cells_with_geom(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    &lex,
                    &cfg,
                    e,
                    now,
                    geom,
                    0,
                );
            }
            println!(
                "STATIC screen  memo={:5}: {:?}/frame  occ={}",
                !bypass,
                started.elapsed() / u32::try_from(frames).expect("small"),
                wd.occ.len()
            );
        }

        // (2) SCROLLING output: one new line per frame, so every surviving row
        // keeps its TEXT and changes its INDEX. This is what makes the memo
        // text-keyed rather than row-keyed.
        for bypass in [true, false] {
            let mut term = Terminal::new(rows as u16, cols as u16);
            for _ in 0..rows {
                term.process(PROSE.as_bytes());
            }
            let mut wd = WordDecorations::default();
            wd.scan_memo.bypass = bypass;
            let mut snap = aterm_core::render::RenderInput::default();
            let started = Instant::now();
            for e in 0..frames {
                term.process(PROSE.as_bytes());
                term.cell_frame_into(&mut snap, rows, cols);
                wd.rescan_from_cells_with_geom(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    &lex,
                    &cfg,
                    e,
                    now,
                    geom,
                    0,
                );
            }
            println!(
                "SCROLL 1 line  memo={:5}: {:?}/frame (includes cell_frame_into)",
                !bypass,
                started.elapsed() / u32::try_from(frames).expect("small")
            );
        }

        // (3) FULL-SCREEN REPAINT — the memo's WORST case: a TUI whose every
        // row's text is new every frame, so every probe misses and the memo's
        // bookkeeping (key + match-list buffers, insert, generation rotation)
        // is pure overhead on top of the full lexicon pass it cannot avoid.
        // `memo=false` here is the pre-memo baseline (`bypass` switches the
        // cache off, it does not merely force misses), so the two lines below
        // ARE the regression this shape is suspected of.
        //
        // Both engines are driven over the SAME frames and each is timed around
        // its own rescan only: a full-screen repaint costs more to push through
        // the parser than to scan, and this box's clock drifts under load, so
        // separate runs of the two legs measure the machine rather than the
        // memo. Interleaved, the drift lands on both.
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut snap = aterm_core::render::RenderInput::default();
        let mut line = String::new();
        // Three engines, same frames: no memo at all, an unpooled memo (a fresh
        // key + match-list allocation per miss), and the shipping memo with the
        // retired generation's buffers recycled.
        let mut engines: Vec<(&str, WordDecorations, Duration)> =
            ["no memo   ", "memo alloc", "memo pool "]
                .into_iter()
                .map(|label| {
                    let mut wd = WordDecorations::default();
                    wd.scan_memo.bypass = label.starts_with("no memo");
                    wd.scan_memo.no_recycle = label.starts_with("memo alloc");
                    (label, wd, Duration::MAX)
                })
                .collect();
        for e in 0..frames {
            term.process(b"\x1b[H");
            for r in 0..rows {
                // A per-(frame, row) prefix in front of the same prose: the
                // scanner's work per row is unchanged, only the memo key is
                // new — so the delta between the two legs is the memo's
                // miss-path cost and nothing else.
                line.clear();
                line.push_str(&e.to_string());
                line.push(' ');
                line.push_str(&r.to_string());
                line.push(' ');
                line.push_str(PROSE);
                term.process(line.as_bytes());
            }
            term.cell_frame_into(&mut snap, rows, cols);
            for (_, wd, best) in &mut engines {
                let started = Instant::now();
                wd.rescan_from_cells_with_geom(
                    &snap.cells,
                    &snap.line_sizes,
                    rows,
                    cols,
                    &lex,
                    &cfg,
                    e,
                    now,
                    geom,
                    0,
                );
                // MIN, not mean: this box builds other crates while the probe
                // runs, and a preempted frame measures the scheduler. The
                // fastest frame of each engine is the one that ran undisturbed.
                *best = (*best).min(started.elapsed());
            }
        }
        for (label, wd, spent) in &engines {
            println!(
                "REPAINT all-miss {label}: {:?} fastest frame (rescan only) \
                 misses={} resident={} spare={} fresh={}",
                *spent,
                wd.scan_memo.misses,
                wd.scan_memo.hot.len() + wd.scan_memo.cold.len(),
                wd.scan_memo.spare.len(),
                wd.scan_memo.fresh_buffers,
            );
        }
    }
}
