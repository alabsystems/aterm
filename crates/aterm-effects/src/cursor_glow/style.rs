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
    /// A wide ribbon in one rolling hue (the whole streak steps round the wheel a little each key; a multi-cell move sweeps half the wheel tail to head) on its own tight bloom stack, plus a shower of night-sky grains: round stardust and glitter, the odd crescent moon and mini-comet, and a dealt four-point star.
    Sparkle,
    /// Black-body warm comet + rising, drifting embers.
    Fire,
    /// Lightning: a monochrome beam in storm violet ([`LASER_DEFAULT_COLOR`]; an explicit `cursor_trail_color` wins), its dim tail brightening to the pure hue at the head around a white-hot filament core, a jagged strike (white-hot main channel, same-hue forks) on a jump and short same-hue crackle arcs off a hot typing run.
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
    pub fn style_names_kitty_pet(s: &str) -> bool {
        matches!(
            spellings::canonical(s),
            Some("rainbow kitty pet" | "rainbow kitty underline" | "rainbow kitty flat")
        )
    }

    /// Whether this style string explicitly asks for the flying kitty head ([`crate::kitty_cursor`]), the rare earned flypast, on the same rainbow ribbon.
    #[must_use]
    #[cfg(test)]
    pub fn style_names_flying_kitty(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty flying")
    }

    /// Whether this style string asks for the pet drawn as a dog ([`crate::kitty_pet::PetSpecies::Dog`]).
    #[inline]
    #[must_use]
    pub fn style_names_dog_pet(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow dog pet")
    }

    /// Whether this style string asks for the full-body pet in any species: the predicate the render path asks; use [`Self::style_names_dog_pet`] after only to pick the skin.
    #[inline]
    #[must_use]
    pub fn style_names_any_pet(s: &str) -> bool {
        matches!(
            spellings::canonical(s),
            Some(
                "rainbow kitty pet"
                    | "rainbow kitty underline"
                    | "rainbow kitty flat"
                    | "rainbow dog pet"
            )
        )
    }

    /// Whether this style string selects the highlighter-plus-under-baseline ribbon: `rainbow kitty underline` and its aliases.
    #[inline]
    #[must_use]
    pub fn style_names_underline_ribbon(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty underline")
    }

    /// Whether this style string selects the flat body (`docs/design/RAINBOW-KITTY-V2.md` §30): the same shape at every cell, the bed alone below the row bottom, a uniform 18 ms edge-in.
    #[inline]
    #[must_use]
    pub fn style_names_flat_ribbon(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty flat")
    }

    /// The resolved style's diagnostic name, printed by `trail status` beside the raw config string so a spelling that fell back to the default reads as a fallback.
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

    /// Whether a resolved `classic` spelling names the mono face (the two-tone `accent → color` tracer) rather than the default spectrum: the old `classic mono` spellings.
    #[inline]
    #[must_use]
    pub fn style_names_classic_mono(style_raw: &str) -> bool {
        let s = style_raw.trim();
        ["classic mono", "classic lumen", "mono classic"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Parse a config string (trimmed, case-insensitive) through [`spellings::canonical`].
    #[inline]
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        spellings::row(s)
            .and_then(|row| row.style)
            .unwrap_or(Self::Lumen)
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
    /// Additive brightness scale 0.0..=1.0 (0 ⇒ effectively off): the visual half, folding the user's brightness knob, the Reduce Motion policy, and the host's load-shed envelope.
    pub intensity: f32,
    /// The audio half: whether a key pressed on this tick's window should be heard.
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
    /// The classic wake's colour face: `false` (plain `classic`) is a rolling spectrum sweep independent of the theme; `true` (`classic mono`) is the two-tone tracer fading `accent → color`, which follows the theme's cursor colour, `cursor_trail_color` and live OSC 12.
    pub classic_mono: bool,
}

/// `Glow size`'s shipped default, in cells: the size every crown is authored at, from which `cursor_trail_radius` scales it.
pub const GLOW_RADIUS_DEFAULT: f32 = 0.6;

/// The shipped `Length` (cells): the knob's default, against which a style whose tail is drawn by distance scales it (sparkle's gold rail).
pub const GLOW_LENGTH_DEFAULT: usize = 24;

/// One tuning knob of the cursor family that a style may or may not read: the `cursor_trail_*` keys the Settings Adjust card lists per style.
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

/// The facts besides the style that decide whether a knob reaches the frame: the ground, the live cursor shape, whether the landing ring is on, and the classic wake's colour face.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KnobContext {
    /// The ground is dark.
    pub dark: bool,
    /// The cursor is a bar: the light rod is its body in every style, and it wears an explicit trail colour.
    pub bar: bool,
    /// `cursor_trail_ring` is on.
    pub ring: bool,
    /// The classic wake draws its one-colour face (a trail colour is set, or the old `classic mono` spelling is saved).
    pub classic_tinted: bool,
    /// The move crosses letters only (a shell's completion over the text it prints): on a dark ground Fire's streak lays no light over a line of text and its meteor ignores `Length`, so the path `Length` cuts shows nowhere.
    pub over_text: bool,
}

impl KnobContext {
    /// The context in which a knob shows at its widest for a style: the ring on and the classic face tinted.
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
    /// THE KNOB LAW: whether a trail drawn in this style reads `knob` in `ctx`.
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

    /// Whether the style draws the trail colour on a block cursor: the auto accent then brightens it, and an explicit colour is pinned against the live cursor's.
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
pub fn style_has_beam(style_raw: &str) -> bool {
    style_has_beam_of(GlowStyle::parse(style_raw.trim()), style_raw)
}

/// [`style_has_beam`] for a caller that already parsed the style (the per-frame `glow_config` path), avoiding a re-parse while sharing one source of truth.
#[inline]
#[must_use]
pub const fn style_has_beam_of(style: GlowStyle, _style_raw: &str) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// The default laser/lightning hue: storm violet (`0x00RRGGBB`), applied when no `cursor_trail_color` is pinned (a night strike is a white-violet flash).
pub const LASER_DEFAULT_COLOR: u32 = 0x00B4_8CFF;

/// The default beam hue: photon ice-blue (`0x00RRGGBB`), applied when no `cursor_trail_color` is set and no cursor colour is pinned (config `cursor_color`, a live OSC 12 — either one recolours the tube), so the tube reads as cool light (a searchlight, not a weapon).
pub const BEAM_DEFAULT_COLOR: u32 = 0x008C_DCFF;

/// The default sparkle hue: starlight gold (`0x00RRGGBB`), applied when no `cursor_trail_color` is set and no cursor colour is pinned (config `cursor_color`, a live OSC 12).
pub const SPARKLE_DEFAULT_COLOR: u32 = 0x00FF_D9A0;

/// The default comet hue: pale glacial blue (`0x00RRGGBB`), applied when no `cursor_trail_color` is set and no cursor colour is pinned (config `cursor_color`, a live OSC 12) — a comet is ice lit white-hot at the nucleus.
pub const COMET_DEFAULT_COLOR: u32 = 0x009E_D6FF;
