// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style resolution: the selectable looks ([`GlowStyle`]), the spelling predicates that read the `spellings` table, and the resolved per-frame [`GlowConfig`].

use super::*;

/// The selectable look of the aurora.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GlowStyle {
    /// Directional tracer in the cursor colour fading to the accent + soft bloom.
    #[default]
    Lumen,
    /// Phaser — a full-spectrum hue sweep along the comet and over time.
    Phaser,
    /// Rainbow kitty: a momentum-driven continuous rainbow ribbon streaming the swept path, brighter and longer with typing heat, with a sparse twinkling starfield.
    RainbowKitty,
    /// Wide ribbon in a rolling hue with a night-sky grain shower.
    Sparkle,
    /// Black-body warm comet + rising, drifting embers.
    Fire,
    /// Storm-violet beam around a white-hot filament core with jump strikes and typing crackle arcs.
    Laser,
    /// Beam: a clean, steady tube of cool light with near-constant power along its length, no particles, scintillation or lightning.
    Beam,
    /// Fluid ocean wake — a deep-blue undertow under a thin travelling cyan crest — with droplets that spray up and arc back down under gravity.
    Water,
    /// Comet: an icy dust tail (white-hot at the head, freezing back through the base hue to a dusty tail) plus twinkling debris glitter shed along the swept path.
    Comet,
    /// The classic wake: a thin four-layer comet under a soft square box-stack bloom, with a landing ring on a jump, and a comet across screen-crossing jumps.
    Classic,
}

impl GlowStyle {
    /// Whether this style string asks for the full-body pet companion ([`crate::kitty_pet`]) rather than the flying kitty.
    #[inline]
    #[must_use]
    pub const fn style_names_kitty_pet(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(
            c.as_bytes(),
            b"rainbow kitty pet" | b"rainbow kitty underline" | b"rainbow kitty flat"
        )
    }

    /// Whether this style string explicitly requests the flying kitty head.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn style_names_flying_kitty(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(c.as_bytes(), b"rainbow kitty flying")
    }

    /// Whether this style string asks for the pet drawn as a dog ([`crate::kitty_pet::PetSpecies::Dog`]).
    #[inline]
    #[must_use]
    pub const fn style_names_dog_pet(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(c.as_bytes(), b"rainbow dog pet")
    }

    /// Whether this style string selects a full-body pet in any species.
    #[inline]
    #[must_use]
    pub const fn style_names_any_pet(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(
            c.as_bytes(),
            b"rainbow kitty pet"
                | b"rainbow kitty underline"
                | b"rainbow kitty flat"
                | b"rainbow dog pet"
        )
    }

    /// Whether this style string selects the highlighter-plus-under-baseline ribbon: `rainbow kitty underline` and its aliases.
    #[inline]
    #[must_use]
    pub const fn style_names_underline_ribbon(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(c.as_bytes(), b"rainbow kitty underline")
    }

    /// Whether this style string selects the flat rainbow ribbon body.
    #[inline]
    #[must_use]
    pub const fn style_names_flat_ribbon(s: &str) -> bool {
        let Some(c) = spellings::canonical(s) else {
            return false;
        };
        matches!(c.as_bytes(), b"rainbow kitty flat")
    }

    /// Diagnostic name of the resolved style.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lumen => "lumen",
            Self::Phaser => "phaser",
            Self::RainbowKitty => "rainbow-kitty",
            Self::Sparkle => "sparkle",
            Self::Fire => "fire",
            Self::Laser => "laser",
            Self::Beam => "beam",
            Self::Water => "water",
            Self::Comet => "comet",
            Self::Classic => "classic",
        }
    }

    /// Whether a `classic` spelling selects the two-tone monochrome face.
    #[inline]
    #[must_use]
    pub const fn style_names_classic_mono(style_raw: &str) -> bool {
        let s = spellings::trim_whitespace_bytes(style_raw);
        spellings::eq_ascii_ci(s, "classic mono")
            || spellings::eq_ascii_ci(s, "classic lumen")
            || spellings::eq_ascii_ci(s, "mono classic")
    }

    /// Parse a config string (trimmed, case-insensitive) through [`spellings::canonical`].
    #[inline]
    #[must_use]
    pub const fn parse(s: &str) -> Self {
        match spellings::row(s) {
            Some(spellings::TrailStyleRow {
                style: Some(style), ..
            }) => *style,
            _ => Self::Lumen,
        }
    }

    /// Whether this style spawns particles (sparks / embers / droplets / laser-ablation sparks / beam stardust / comet debris glitter).
    #[inline]
    pub(super) const fn has_particles(self) -> bool {
        matches!(
            self,
            Self::Sparkle | Self::Fire | Self::Water | Self::Laser | Self::Beam | Self::Comet
        )
    }
}

/// Resolved tunables (`Copy`, so the host reads it out before borrowing window state).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowConfig {
    pub enabled: bool,
    pub style: GlowStyle,
    /// Base hue `0x00RRGGBB` (comet head + bloom), default theme cursor.
    pub color: u32,
    /// Secondary hue `0x00RRGGBB` (comet tail + ring), default cursor brightened.
    pub accent: u32,
    /// Comet fade duration.
    pub duration: Duration,
    /// Max comet length in cells.
    pub length: usize,
    /// Additive brightness scale `0.0..=1.0` (`0` = off).
    pub intensity: f32,
    /// Whether a key pressed on this tick's window should be heard.
    pub audible: bool,
    /// Bloom-crown radius in cells (0 ⇒ no crown, comet only).
    pub radius: f32,
    /// Landing-ring "ping" on a jump.
    pub ring: bool,
    /// Whether the theme background is dark.
    pub dark_theme: bool,
    /// The theme's resolved default foreground and background, `0x00RRGGBB`, with OSC 10/11 and DECSCNM folded in by the host.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether this style draws its own additive comet beam (the anti-aliased streak along the swept path).
    pub beam: bool,
    /// Horizontal attach point of the live-cursor bridge as a fraction of cell width (`0.5` = centre).
    pub head_dx: f32,
    /// The rainbow ribbon's dark-theme transverse presentation: tall body or explicit underline/highlighter.
    pub ribbon_tall: bool,
    /// The flat body, selected only by the explicit `… flat` spelling ([`GlowStyle::style_names_flat_ribbon`]).
    pub ribbon_flat: bool,
    /// Whether the classic wake uses the two-tone tracer (`true`) instead of a rolling spectrum (`false`).
    pub classic_mono: bool,
    /// The light the frame draws its light arms by (`docs/design/PRISM-GLASS.md` §15.0.4): the
    /// host's latched light scheme with the live phase level; [`Light::OFF`] on every dark ground,
    /// and until a host latches one.
    pub light: Light,
}

/// The Prism Glass phase levels, in the order they go live (§15.0.4): each phase's last commit
/// raises [`crate::rainbow_kitty::prism_glass::LIVE`] one level, and a level draws every arm of
/// the levels before it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Live {
    /// No light arm draws: a latched light scheme keeps main's paper arm.
    Off,
    /// Phase 1: the glass body.
    Glass,
    /// Phase 2: the lip.
    Lip,
    /// Phase 3: the marks (stars, caret, cat).
    Marks,
    /// Phase 4: the vanish fixes.
    Vanish,
}

impl Live {
    /// The level's word, as the trail-diff dump's `live <level>` and `trail light` spell it.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Glass => "glass",
            Self::Lip => "lip",
            Self::Marks => "marks",
            Self::Vanish => "vanish",
        }
    }
}

/// What a frame's light arms read (§15.0.4): whether the host latched a light configured scheme
/// (`on`, the spec's `light_on`) and which phases are live on it. Every light arm reads one of
/// [`Self::glass`], [`Self::lip`], [`Self::marks`] or [`Self::vanish`], each false on
/// [`Self::OFF`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Light(u8);

impl Light {
    /// No light scheme latched: main's frame.
    pub const OFF: Self = Self(0);
    const ON: u8 = 1;
    const GLASS: u8 = 1 << 1;
    const LIP: u8 = 1 << 2;
    const MARKS: u8 = 1 << 3;
    const VANISH: u8 = 1 << 4;
    const SHEER: u8 = 1 << 5;

    /// A latched light scheme at the live level `live`: the arms of every level up to `live`.
    #[inline]
    #[must_use]
    pub const fn at(live: Live) -> Self {
        Self(
            Self::ON
                | match live {
                    Live::Off => 0,
                    Live::Glass => Self::GLASS,
                    Live::Lip => Self::GLASS | Self::LIP,
                    Live::Marks => Self::GLASS | Self::LIP | Self::MARKS,
                    Live::Vanish => Self::GLASS | Self::LIP | Self::MARKS | Self::VANISH,
                },
        )
    }

    /// The frame's light from its latch's verdict (§15.2 1.3): [`Self::at`] `live` when the latch
    /// admitted the frame, [`Self::OFF`] when it did not.
    #[inline]
    #[must_use]
    pub const fn admitted(on: bool, live: Live) -> Self {
        if on { Self::at(live) } else { Self::OFF }
    }

    /// The bits, as the trail-diff dump prints them (`light=`).
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// A light configured scheme is latched (the spec's `light_on`), whatever is live on it.
    #[inline]
    #[must_use]
    pub const fn on(self) -> bool {
        self.0 & Self::ON != 0
    }

    #[inline]
    const fn has(self, arm: u8) -> bool {
        self.0 & (Self::ON | arm) == Self::ON | arm
    }

    /// This light on its page (§15.2 1.7, `host::PageFacts`): a latched light over a page that is
    /// not `clear` (a translucent window, or a wallpaper or an image under the text) is sheer, and
    /// a sheer light draws no glass, so the frame draws main's paper arm there. [`Self::OFF`]
    /// stays [`Self::OFF`] on every page.
    #[inline]
    #[must_use]
    pub const fn on_page(self, clear: bool) -> Self {
        if self.on() && !clear {
            Self(self.0 | Self::SHEER)
        } else {
            self
        }
    }

    /// The light lies over a see-through page ([`Self::on_page`]).
    #[inline]
    #[must_use]
    pub const fn sheer(self) -> bool {
        self.0 & Self::SHEER != 0
    }

    /// The glass body draws (Phase 1): its level is live and the page is clear ([`Self::sheer`]).
    #[inline]
    #[must_use]
    pub const fn glass(self) -> bool {
        self.has(Self::GLASS) && !self.sheer()
    }

    /// The lip draws (Phase 2).
    #[inline]
    #[must_use]
    pub const fn lip(self) -> bool {
        self.has(Self::LIP)
    }

    /// The marks draw (Phase 3).
    #[inline]
    #[must_use]
    pub const fn marks(self) -> bool {
        self.has(Self::MARKS)
    }

    /// The vanish fixes draw (Phase 4).
    #[inline]
    #[must_use]
    pub const fn vanish(self) -> bool {
        self.has(Self::VANISH)
    }
}

impl GlowConfig {
    /// A config with every field set, for a test that needs one whole: it changes the fields it
    /// is about by struct update, so a field a later phase adds gets its value here and no test
    /// literal has to name every field (the trail-diff closure's tests among them, §15.0.6).
    #[cfg(test)]
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub const fn sample_for_test() -> Self {
        Self {
            enabled: true,
            style: GlowStyle::RainbowKitty,
            color: 0x0012_3456,
            accent: 0x0065_4321,
            duration: Duration::from_millis(260),
            length: 24,
            intensity: 0.7,
            audible: true,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00DD_DDDD,
            theme_bg: 0x0011_1111,
            beam: true,
            head_dx: 0.5,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
            light: Light::OFF,
        }
    }
}

/// The shipped resolve: what a window with no config resolves on the cold dark ground (the
/// shipped `rainbow kitty pet`, the default theme's cursor colour, every knob unset), so a test
/// that names only the fields it is about takes the rest from here (§15.1 P0.1).
impl Default for GlowConfig {
    #[inline]
    fn default() -> Self {
        Self {
            enabled: true,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x0078_FFB8,
            duration: Duration::from_millis(260),
            length: GLOW_LENGTH_DEFAULT,
            intensity: 1.0,
            audible: false,
            radius: 0.0,
            ring: false,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            beam: false,
            head_dx: 0.5,
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
            light: Light::OFF,
        }
    }
}

/// Default glow crown radius in cells.
pub const GLOW_RADIUS_DEFAULT: f32 = 0.6;

/// Default maximum trail length in cells.
pub const GLOW_LENGTH_DEFAULT: usize = 24;

/// Cursor trail tuning knob that a style may read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrailKnob {
    /// `cursor_trail_length`: how many cells one move's trail may span.
    Length,
    /// `cursor_trail_radius` (Glow size): the size of the crown at the caret.
    Radius,
    /// `cursor_trail_ring`: the landing ring on a jump.
    Ring,
    /// `cursor_trail_color`.
    Color,
    /// `cursor_trail_accent`.
    Accent,
    /// `cursor_fire_shimmer`: the GPU heat shimmer, which distorts the fire field only Fire emits.
    FireShimmer,
}

/// Context flags that determine whether a [`TrailKnob`] affects a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KnobContext {
    /// The ground is dark.
    pub dark: bool,
    /// The cursor is a bar: the light rod is its body in every style, and it wears an explicit trail colour.
    pub bar: bool,
    /// `cursor_trail_ring` is on.
    pub ring: bool,
    /// The classic wake draws its one-colour face (a trail colour is set, or the old `classic mono` spelling is saved).
    pub classic_tinted: bool,
    /// The move hops across text cells only, on its own row and short of a leap (a jump
    /// [`GlowStyle::jump_reach`] calls one flies over whatever it crosses).
    pub over_text: bool,
}

impl Default for KnobContext {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl KnobContext {
    /// Zeroed knob context.
    pub const EMPTY: Self = Self {
        dark: false,
        bar: false,
        ring: false,
        classic_tinted: false,
        over_text: false,
    };

    /// Context where knobs apply most broadly for `bar`.
    #[inline]
    #[must_use]
    pub const fn widest(bar: bool) -> Self {
        Self {
            dark: true,
            bar,
            ring: true,
            classic_tinted: true,
            over_text: false,
        }
    }
}

impl GlowStyle {
    /// Whether a trail drawn in this style reads `knob` in `ctx`.
    #[inline]
    #[must_use]
    pub const fn reads_knob(self, knob: TrailKnob, ctx: KnobContext) -> bool {
        use GlowStyle::{
            Beam, Classic, Comet, Fire, Laser, Lumen, Phaser, RainbowKitty, Sparkle, Water,
        };
        match knob {
            TrailKnob::Length => match self {
                RainbowKitty | Laser | Beam | Water => false,
                Fire => !(ctx.over_text && ctx.dark),
                Lumen | Sparkle | Phaser | Comet | Classic => true,
            },
            TrailKnob::Radius if !ctx.dark => matches!(self, Lumen | Classic),
            TrailKnob::Radius | TrailKnob::Ring => !matches!(self, RainbowKitty | Beam),
            TrailKnob::Color => ctx.bar || !matches!(self, RainbowKitty | Fire | Water | Phaser),
            TrailKnob::Accent => match self {
                Lumen | Comet => ctx.dark || ctx.ring,
                Classic => ctx.ring || ctx.classic_tinted,
                Sparkle | Phaser => ctx.ring,
                RainbowKitty | Fire | Laser | Beam | Water => false,
            },
            TrailKnob::FireShimmer => matches!(self, Fire),
        }
    }

    /// The Trail length from which a licensed jump draws alike at every longer length (`None`:
    /// Length shapes nothing it draws). The jump moves `rows` rows and `cols` columns across
    /// cells `aspect` times as tall as they are wide, onto the ground `ctx` names: along its row,
    /// or off it as an Enter does, much farther across than down. Length cuts the
    /// path a jump lays along its longer axis, from the cell it leaves to the one before the
    /// caret: most styles drop the cell it leaves, where Classic's wake and Comet's trail cells
    /// keep it. Phaser lays none off its row; Fire flies a leap (off its row, or of
    /// `METEOR_MIN_COLS` columns or more) as a meteor whose streak leans no shallower than
    /// `METEOR_SLOPE`; and on a dark ground Length also grows Sparkle's bed under a jump, up to
    /// `SPARKLE_LEAP_WEDGE_GROWTH` times the default length.
    #[must_use]
    #[inline]
    pub const fn jump_reach(
        self,
        rows: u16,
        cols: u16,
        aspect: f32,
        ctx: KnobContext,
    ) -> Option<f32> {
        let (dr, dc) = (rows as f32, cols as f32);
        let leap = rows > 0 || (cols as i32) >= CursorGlow::METEOR_MIN_COLS;
        let path = match self {
            GlowStyle::Classic | GlowStyle::Comet => dr.max(dc),
            _ => dr.max(dc) - 1.0,
        };
        match self {
            GlowStyle::Fire if leap && rows > 0 => {
                Some(dc.min(CursorGlow::METEOR_SLOPE * dr * aspect).max(dr))
            }
            GlowStyle::Fire if leap => Some(dc),
            GlowStyle::Phaser if rows > 0 => None,
            GlowStyle::Sparkle if ctx.dark => {
                Some(path.max(CursorGlow::SPARKLE_LEAP_WEDGE_GROWTH * GLOW_LENGTH_DEFAULT as f32))
            }
            _ if self.reads_knob(TrailKnob::Length, ctx) => Some(path),
            _ => None,
        }
    }

    /// Whether this style draws the trail colour on a block cursor.
    #[inline]
    #[must_use]
    pub const fn draws_trail_color(self) -> bool {
        self.reads_knob(
            TrailKnob::Color,
            KnobContext {
                dark: true,
                bar: false,
                ring: false,
                classic_tinted: false,
                over_text: false,
            },
        )
    }
}

/// Whether a raw `cursor_trail_style` string draws its own additive comet beam.
#[inline]
#[must_use]
pub const fn style_has_beam(style_raw: &str) -> bool {
    style_has_beam_of(GlowStyle::parse(style_raw), style_raw)
}

/// [`style_has_beam`] for an already-parsed [`GlowStyle`].
#[inline]
#[must_use]
pub const fn style_has_beam_of(style: GlowStyle, _style_raw: &str) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// Default laser/lightning hue (`0x00RRGGBB`).
pub const LASER_DEFAULT_COLOR: u32 = 0x00B4_8CFF;

/// Default beam hue (`0x00RRGGBB`).
pub const BEAM_DEFAULT_COLOR: u32 = 0x008C_DCFF;

/// Default sparkle hue (`0x00RRGGBB`).
pub const SPARKLE_DEFAULT_COLOR: u32 = 0x00FF_D9A0;

/// Default comet hue (`0x00RRGGBB`).
pub const COMET_DEFAULT_COLOR: u32 = 0x009E_D6FF;
