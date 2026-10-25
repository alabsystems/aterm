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
    /// The move crosses text cells only.
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
