// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-native live demonstrations for Settings.
//!
//! A preview is a first-class [`crate::native_ui::UiContent`] node. Its terminal
//! typography uses the renderer-injected monospace face, while cursor motion is
//! produced by the same clockless `aterm-effects` cursor-family step
//! ([`CursorFx`]) a live terminal runs and composited by the shipping renderer
//! on the specimen's own cell grid. The fixed sample and scripted typing are
//! bounded and deterministic; no PTY, platform widget, web view, filesystem
//! access, or wall-clock decision is involved.
//!
//! Optional display-headroom effects are represented by the same bounded SDR
//! tone-map used by capture, so the semantic preview remains portable and
//! truthful on hosts without an HDR panel.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_effects::cursor_fx::{CursorFx, CursorFxInput};
use aterm_grapheme::GraphemeClusters;
#[cfg(test)]
use aterm_grapheme::grapheme_display_width;
use aterm_render::{GlowQuad, HaloMode, RainHalo, RenderInput, Theme};

use crate::cursor_glow::{Geom, GlowStyle, TrailParams};
use crate::cursor_trail::TrailConfig;
use crate::native_ui::{Insets, Layout, Length, LogicalRect, UiContent, UiNode};
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{
    DrawPrim, SemanticFontCandidate, SemanticVariation, SpecimenTextBlending, TerminalSpecimenSpec,
    TextFace, TextWeight, rgba, text_prim,
};

const MIN_DURATION_MS: u64 = 30;
const MAX_DURATION_MS: u64 = 2_000;
const MAX_LENGTH: usize = 512;
const PHASE_CYCLE_MS: u64 = 2_400;
const MOTION_STEP_MS: u64 = 52;
const CURSOR_BLINK_HALF_MS: u64 = 530;

/// The narrowest host cadence that can change a preview's pixels.
///
/// Keeping this decision on the normalized semantic preview spec means paint,
/// retained-raster identity, and the event-loop timer cannot independently
/// guess whether a demonstration is moving. `BlinkEdge` is deliberately a
/// one-shot deadline rather than a frame cadence: a blinking cursor has only
/// two visual states and must not wake/rasterize at 30 fps between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewAnimation {
    #[default]
    None,
    BlinkEdge {
        after_ms: u64,
    },
    Continuous,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewScene {
    Appearance,
    Typography,
    #[default]
    CursorMotion,
}

impl PreviewScene {
    const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance preview",
            Self::Typography => "Typography preview",
            Self::CursorMotion => "Cursor and motion preview",
        }
    }

    const fn badge_label(self) -> &'static str {
        match self {
            Self::Appearance => "COLOR PREVIEW",
            Self::Typography => "TEXT PREVIEW",
            Self::CursorMotion => "CURSOR PREVIEW",
        }
    }
}

/// Why a moving preview stands still, in the words the preview speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum PreviewPause {
    /// Reduced motion with no recorded cause.
    #[default]
    Unspecified,
    SeriousMode,
    /// Adaptive motion is easing effects off under load.
    Load,
    /// The window is in the background.
    Unfocused,
    /// The Motion setting is Reduce motion.
    MotionSetting,
    /// Motion follows the system, and the system asks for reduced motion.
    SystemReduceMotion,
}

impl PreviewPause {
    /// The person's own choice is named in the Motion picker's words.
    fn because(self) -> String {
        match self {
            Self::Unspecified => "motion is reduced".to_string(),
            Self::SeriousMode => "Serious Mode is on".to_string(),
            Self::Load => "aterm is easing effects off under load".to_string(),
            Self::Unfocused => "the window is in the background".to_string(),
            Self::MotionSetting => format!(
                "Motion is set to {}",
                crate::native_settings::reduce_motion_words()
            ),
            Self::SystemReduceMotion => "the system asks for reduced motion".to_string(),
        }
    }

    fn badge(self) -> String {
        match self {
            Self::SeriousMode => "Serious Mode".to_string(),
            Self::Unfocused | Self::Load => "Paused".to_string(),
            Self::MotionSetting => crate::native_settings::reduce_motion_words(),
            Self::SystemReduceMotion => "System Reduce Motion".to_string(),
            Self::Unspecified => "Reduced motion".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewCursorStyle {
    #[default]
    Block,
    Bar,
}

impl PreviewCursorStyle {
    const fn label(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Bar => "bar",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewTrailStyle {
    Off,
    Lumen,
    Phaser,
    RainbowKitty,
    Sparkle,
    Fire,
    Laser,
    Water,
    Beam,
    Comet,
    /// THE CLASSIC WAKE — the salvaged v0.28 trail.
    Classic,
    /// A loaded user-authored Trail Pack. Its actual data travels separately in
    /// [`CursorPreviewSpec::trail_pack`], so two packs remain distinguishable
    /// even though both use the shared engine's `GlowStyle::Custom` dispatch.
    Custom,
}

/// The cursor companion selected by a rainbow-family style spelling.
///
/// Companion identity deliberately rides beside [`PreviewTrailStyle`]: the
/// effects engine resolves every one of these choices to
/// [`GlowStyle::RainbowKitty`], while the runtime uses the original style
/// token to choose the resident cat, resident dog, or flying kitty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewTrailCompanion {
    #[default]
    None,
    Pet(aterm_effects::kitty_pet::PetSpecies),
    FlyingKitty,
}

/// The catalog a preview resolves a trail spelling against: always EMPTY, since a
/// `pack:` preview carries its resolved data in [`CursorPreviewSpec::trail_pack`].
/// Built once — resolution runs several times per presented frame, and a fresh
/// catalog is a heap allocation each time.
fn no_trail_packs() -> &'static crate::app_config::TrailPackCatalog {
    static NO_TRAIL_PACKS: std::sync::LazyLock<crate::app_config::TrailPackCatalog> =
        std::sync::LazyLock::new(Default::default);
    &NO_TRAIL_PACKS
}

impl PreviewTrailStyle {
    #[cfg(test)]
    pub(crate) fn parse(value: &str) -> Self {
        Self::from_resolution(
            value,
            crate::app_config::resolve_trail_style(value, no_trail_packs()),
        )
    }

    pub(crate) fn from_resolution(
        raw: &str,
        resolved: crate::app_config::ResolvedTrailStyle,
    ) -> Self {
        match resolved.style {
            Some(GlowStyle::Lumen) => Self::Lumen,
            Some(GlowStyle::Phaser) => Self::Phaser,
            Some(GlowStyle::RainbowKitty) => Self::RainbowKitty,
            Some(GlowStyle::Sparkle) => Self::Sparkle,
            Some(GlowStyle::Fire) => Self::Fire,
            Some(GlowStyle::Laser) => Self::Laser,
            Some(GlowStyle::Water) => Self::Water,
            Some(GlowStyle::Beam) => Self::Beam,
            Some(GlowStyle::Comet) => Self::Comet,
            Some(GlowStyle::Classic) => Self::Classic,
            Some(GlowStyle::Custom) => Self::Custom,
            // `resolve_trail_style` answers no style only for `off` and for a
            // `pack:` it could not load; an unloaded pack previews as unavailable.
            None if raw.trim().starts_with("pack:") => Self::Custom,
            None => Self::Off,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lumen => "lumen",
            Self::Phaser => "phaser",
            Self::RainbowKitty => "rainbow kitty",
            Self::Sparkle => "sparkle",
            Self::Fire => "fire",
            Self::Laser => "laser",
            Self::Water => "water",
            Self::Beam => "beam",
            Self::Comet => "comet",
            Self::Classic => "classic",
            Self::Custom => "custom pack",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CursorPreviewSpec {
    pub(crate) style: PreviewCursorStyle,
    pub(crate) blink: bool,
    pub(crate) trail_enabled: bool,
    pub(crate) trail_style: PreviewTrailStyle,
    /// The selected preference token, retained because rainbow-family
    /// geometry and companion identity intentionally live in its spelling.
    /// `trail_style` is only the shared engine's broad dispatch class.
    pub(crate) trail_style_raw: Arc<str>,
    /// Resolved, validated custom interpreter data. `None` for every built-in
    /// and for an unavailable `pack:<id>`; the latter fails closed without
    /// emitting geometry or scheduling an invisible animation cadence.
    pub(crate) trail_pack: Option<Arc<TrailParams>>,
    /// A loaded pack's own name from its manifest, so the preview says
    /// "Emberfall" as the pack's row does; `None` for a built-in style.
    pub(crate) trail_pack_name: Option<Arc<str>>,
    /// Optional packed `0x00RRGGBB`; `None` follows the live theme/style rule.
    pub(crate) color: Option<u32>,
    /// Optional packed `0x00RRGGBB`; `None` brightens the resolved base colour.
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
    /// The typing-momentum glow switch: the scripted typing warms the caret
    /// exactly as real keys do.
    pub(crate) momentum_glow: bool,
    /// The cursor colour was chosen rather than inherited from the theme; a
    /// rainbow or phaser caret builds from it instead of from white.
    pub(crate) cursor_color_pinned: bool,
}

impl Default for CursorPreviewSpec {
    fn default() -> Self {
        Self {
            style: PreviewCursorStyle::Block,
            blink: true,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::RainbowKitty,
            trail_style_raw: Arc::from(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            trail_pack: None,
            trail_pack_name: None,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 1.0,
            radius: 0.6,
            ring: true,
            momentum_glow: true,
            cursor_color_pinned: false,
        }
    }
}

impl CursorPreviewSpec {
    fn normalized(&self) -> Self {
        let trimmed_style = self.trail_style_raw.trim();
        let trail_style_raw = if trimmed_style.len() == self.trail_style_raw.len() {
            Arc::clone(&self.trail_style_raw)
        } else {
            Arc::from(trimmed_style)
        };
        Self {
            style: self.style,
            blink: self.blink,
            trail_enabled: self.trail_enabled,
            trail_style: self.trail_style,
            trail_style_raw,
            trail_pack: if matches!(self.trail_style, PreviewTrailStyle::Custom) {
                self.trail_pack.clone()
            } else {
                None
            },
            trail_pack_name: if matches!(self.trail_style, PreviewTrailStyle::Custom) {
                self.trail_pack_name.clone()
            } else {
                None
            },
            color: self.color.map(|color| color & 0x00ff_ffff),
            accent: self.accent.map(|color| color & 0x00ff_ffff),
            duration_ms: self.duration_ms.clamp(MIN_DURATION_MS, MAX_DURATION_MS),
            length: self.length.clamp(1, MAX_LENGTH),
            intensity: finite_or(self.intensity, 0.0).clamp(0.0, 1.0),
            radius: finite_or(self.radius, 0.0).clamp(0.0, 2.0),
            ring: self.ring,
            momentum_glow: self.momentum_glow,
            cursor_color_pinned: self.cursor_color_pinned,
        }
    }

    /// The selected token when it still names this broad preview style.
    ///
    /// The fallback keeps direct
    /// `CursorPreviewSpec { trail_style, ..Default::default() }` callers honest:
    /// changing the broad style without also changing its raw
    /// token must not accidentally retain the default rainbow-pet semantics.
    ///
    /// The trail's name as the Cursor Styles list says it: a loaded pack by
    /// its manifest's name, every other style by its row's.
    pub(crate) fn trail_style_label(&self) -> String {
        match (&self.trail_pack_name, self.trail_style) {
            (Some(name), PreviewTrailStyle::Custom) => name.to_string(),
            _ => crate::native_settings::setting_choice_label(
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                self.effective_trail_style_raw(),
            ),
        }
    }

    /// AUTHORED, not resolved: this is the text the badge shows and the cache
    /// keys on, so it keeps the user's spelling and case.
    pub(crate) fn effective_trail_style_raw(&self) -> &str {
        let raw = self.trail_style_raw.trim();
        let resolved = crate::app_config::resolve_trail_style(raw, no_trail_packs());
        if PreviewTrailStyle::from_resolution(raw, resolved) == self.trail_style {
            raw
        } else {
            self.trail_style.label()
        }
    }

    /// [`Self::effective_trail_style_raw`] as the RUNTIME resolves it — the
    /// spelling the host's raw-token predicates would be asked about, so an
    /// unrecognized value previews the default's companion and ribbon instead
    /// of the geometry of a look nothing renders.
    #[cfg(test)]
    fn effective_trail_style_token(&self) -> &str {
        crate::app_config::effective_trail_style_token(self.effective_trail_style_raw())
    }

    fn resolved_trail_style(&self) -> crate::app_config::ResolvedTrailStyle {
        if self.trail_style == PreviewTrailStyle::Custom {
            return crate::app_config::ResolvedTrailStyle {
                canonical: None,
                style: self.trail_pack.as_ref().map(|_| GlowStyle::Custom),
                pack: self.trail_pack.as_deref().copied(),
                issue: self
                    .trail_pack
                    .is_none()
                    .then_some(crate::app_config::TrailStyleIssue::MissingPack),
            };
        }
        crate::app_config::resolve_trail_style(self.effective_trail_style_raw(), no_trail_packs())
    }

    fn resolved_trail_presentation(&self) -> crate::app_config::ResolvedTrailPresentation {
        let raw = self.effective_trail_style_raw();
        crate::app_config::resolve_trail_presentation_from_style(raw, self.resolved_trail_style())
    }

    /// Match the runtime's broad-style gate plus its raw-token companion
    /// predicates. A non-pet rainbow spelling owns the flying kitty — the
    /// historical `nyan` / `rainbow` aliases and the explicit `… flying`
    /// spellings.
    ///
    /// The GEOMETRY-ONLY underline variant used to land here too, and this
    /// comment used to say so. It was not a rule: `style_names_kitty_pet` is a
    /// whole-string equality and `"rainbow kitty underline"` simply matched no
    /// entry, so one appended geometry word swapped the animal. It draws the
    /// resident now, exactly like the `rainbow kitty` it is a geometry of; this
    /// preview follows because it asks the engine's own predicate.
    pub(crate) fn trail_companion(&self) -> PreviewTrailCompanion {
        let presentation = self.resolved_trail_presentation();
        if presentation.style.style != Some(GlowStyle::RainbowKitty) {
            return PreviewTrailCompanion::None;
        }
        if let Some(species) = presentation.pet_species {
            PreviewTrailCompanion::Pet(species)
        } else {
            PreviewTrailCompanion::FlyingKitty
        }
    }

    fn glow_config(&self, theme: Theme, head_dx: f32) -> crate::cursor_glow::GlowConfig {
        let presentation = self.resolved_trail_presentation();
        crate::app_config::resolve_cursor_glow(
            crate::app_config::CursorGlowInputs {
                enabled: self.trail_enabled,
                color: self.color,
                accent: self.accent,
                duration_ms: self.duration_ms,
                length: self.length,
                intensity: self.intensity,
                radius: self.radius,
                ring: self.ring,
            },
            presentation,
            theme.cursor,
            theme_bg_is_dark(theme.bg),
            // The preview HAS a theme in scope and no Terminal, so there is
            // nothing to fold: no OSC 10/11 and no DECSCNM on this path.
            theme.fg & 0x00FF_FFFF,
            theme.bg & 0x00FF_FFFF,
            head_dx,
        )
    }

    const fn trail_pack_unavailable(&self) -> bool {
        matches!(self.trail_style, PreviewTrailStyle::Custom) && self.trail_pack.is_none()
    }

    const fn has_resolved_trail(&self) -> bool {
        !matches!(self.trail_style, PreviewTrailStyle::Off) && !self.trail_pack_unavailable()
    }
}

/// What the typing-momentum glow shows on a preview ([`SettingsPreviewSpec::typing_glow`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypingGlow {
    Halo,
    Tint,
    Nothing,
}

/// A bounded terminal-canvas palette used to preview an uncommitted theme.
///
/// Values are packed `0x00RRGGBB`. The override is intentionally narrower
/// than [`Theme`]: it cannot alter Settings chrome, layout, typography, or
/// interaction state, and therefore remains portable across every semantic
/// renderer host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PreviewTerminalTheme {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) cursor: u32,
    pub(crate) selection: u32,
    pub(crate) ansi: [u32; 16],
}

impl PreviewTerminalTheme {
    /// Every slot is a real palette: there is no stand-in ANSI table, so a
    /// caller with only a renderer [`Theme`] (four colours) cannot build a
    /// preview that draws colours the terminal never uses.
    pub(crate) const fn new(
        fg: u32,
        bg: u32,
        cursor: u32,
        selection: u32,
        ansi: [u32; 16],
    ) -> Self {
        let mut masked = [0_u32; 16];
        let mut index = 0;
        while index < 16 {
            masked[index] = ansi[index] & 0x00ff_ffff;
            index += 1;
        }
        Self {
            fg: fg & 0x00ff_ffff,
            bg: bg & 0x00ff_ffff,
            cursor: cursor & 0x00ff_ffff,
            selection: selection & 0x00ff_ffff,
            ansi: masked,
        }
    }

    pub(crate) fn from_scheme(scheme: &aterm_types::scheme::ColorScheme) -> Self {
        let parts = scheme.to_theme_parts();
        Self::new(
            parts.fg,
            parts.bg,
            parts.cursor,
            parts.selection,
            scheme.ansi.map(pack_rgb),
        )
    }

    /// The colours a resolution draws: the selected theme with every
    /// `aterm.toml` colour override (`palette[]` included) already applied.
    pub(crate) fn from_resolved(resolved: &crate::resolved_colors::ResolvedColors) -> Self {
        Self::from_scheme(&resolved.scheme)
    }

    /// `host`'s four colours over the ANSI table the specimen's engine draws
    /// when it is given no palette ([`aterm_types::ColorPalette::default_color`]),
    /// so the swatches and the specimen agree for a spec with no candidate.
    fn over_engine_palette(host: Theme) -> Self {
        let ansi = std::array::from_fn(|index| {
            pack_rgb(aterm_types::ColorPalette::default_color(index as u8))
        });
        Self::new(host.fg, host.bg, host.cursor, host.selection, ansi)
    }

    /// `self` with every slot masked to `0x00RRGGBB`.
    const fn normalized(self) -> Self {
        Self::new(self.fg, self.bg, self.cursor, self.selection, self.ansi)
    }

    const fn into_theme(self) -> Theme {
        Theme {
            fg: self.fg,
            bg: self.bg,
            cursor: self.cursor,
            selection: self.selection,
        }
    }

    fn semantic_description(self) -> String {
        format!(
            "candidate terminal theme: foreground #{:06X}, background #{:06X}, cursor #{:06X}, selection #{:06X}",
            self.fg, self.bg, self.cursor, self.selection
        )
    }
}

/// Who the candidate palette is, for the words a screen reader speaks: the
/// theme's label as people see it and the appearance its palette reads as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewThemeIdentity {
    pub(crate) label: String,
    pub(crate) light: bool,
}

impl PreviewThemeIdentity {
    /// `resolved`'s label (a file's `name =`, a built-in's name) and the
    /// appearance it reports.
    pub(crate) fn from_resolved(
        resolved: &crate::resolved_colors::ResolvedColors,
        themes: &crate::app_config::ThemeCatalog,
    ) -> Self {
        Self {
            label: themes
                .meta(&resolved.id)
                .map_or_else(|| resolved.id.clone(), |meta| meta.display_name),
            light: resolved.reported == aterm_types::Appearance::Light,
        }
    }
}

/// The WCAG floor below which an ANSI colour is hard to read on the background.
const LEGIBLE_ANSI_CONTRAST: f64 = 3.0;

/// The accessible summary of a candidate palette: `<label>, light|dark, text
/// contrast X:1, K colours below 3:1`.
fn spoken_palette(theme: PreviewTerminalTheme, identity: Option<&PreviewThemeIdentity>) -> String {
    let rgb = |packed: u32| {
        let c = packed_rgb(packed);
        [c.r, c.g, c.b]
    };
    let bg = rgb(theme.bg);
    let (label, light) = identity.map_or_else(
        || ("Custom colours", aterm_messages::ink::bg_is_light(bg)),
        |identity| (identity.label.as_str(), identity.light),
    );
    let text = aterm_messages::ink::contrast(rgb(theme.fg), bg);
    let illegible = theme
        .ansi
        .iter()
        .filter(|&&color| aterm_messages::ink::contrast(rgb(color), bg) < LEGIBLE_ANSI_CONTRAST)
        .count();
    format!(
        "{label}, {}, text contrast {}:1, {illegible} {} below 3:1",
        if light { "light" } else { "dark" },
        trim_contrast(text),
        if illegible == 1 { "colour" } else { "colours" },
    )
}

/// A contrast ratio to one decimal, rounded down so a floor is never
/// overstated (2.96 reads 2.9, not 3).
fn trim_contrast(ratio: f64) -> String {
    let tenths = (ratio * 10.0).floor() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{tenths:.0}")
    } else {
        format!("{tenths:.1}")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppearancePreviewSpec {
    pub(crate) window_theme: String,
    pub(crate) minimum_contrast: f32,
    pub(crate) selection_foreground: Option<u32>,
    pub(crate) selection_inactive: bool,
    pub(crate) bold_is_bright: bool,
    pub(crate) faint_opacity: f32,
}

impl Default for AppearancePreviewSpec {
    fn default() -> Self {
        Self {
            window_theme: "auto".to_string(),
            minimum_contrast: 1.0,
            selection_foreground: None,
            selection_inactive: false,
            bold_is_bright: true,
            faint_opacity: 0.5,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypographyPreviewSpec {
    pub(crate) cursor_break_ligatures: bool,
    pub(crate) underline_position: i32,
    pub(crate) underline_thickness: i32,
    pub(crate) underline_skip_descenders: bool,
    pub(crate) text_blending: SpecimenTextBlending,
    pub(crate) font_thicken: bool,
    pub(crate) stem_gamma: f32,
    pub(crate) variations: Vec<SemanticVariation>,
}

impl Default for TypographyPreviewSpec {
    fn default() -> Self {
        Self {
            cursor_break_ligatures: true,
            underline_position: 0,
            underline_thickness: 0,
            underline_skip_descenders: true,
            text_blending: SpecimenTextBlending::LinearCorrected,
            font_thicken: false,
            stem_gamma: 1.0,
            variations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CursorPostFxSpec {
    /// Authored candidate string shown literally in introspection.
    pub(crate) kitty_sprite: String,
    /// Immutable, already-decoded asset from the same config snapshot as the
    /// view. Paint consumes this Arc-backed value and never opens a path.
    pub(crate) kitty_asset: crate::app_config::KittySpriteAsset,
    pub(crate) bloom: bool,
    pub(crate) bloom_strength: f32,
    pub(crate) bloom_radius: f32,
    pub(crate) fire_shimmer: bool,
    pub(crate) hdr_glow: bool,
    pub(crate) sdr_boost: f32,
    pub(crate) motion_raw: String,
    pub(crate) motion_effective: String,
    pub(crate) motion_reason: String,
    pub(crate) adaptive_motion: bool,
    pub(crate) performance_reduced: bool,
    /// The cat the window's companion wears now: the preview's cat, dog and
    /// flying kitty wear its coat and eyes, as the terminal's do.
    pub(crate) companion_look: aterm_effects::kitty_registry::KittyLook,
}

impl Default for CursorPostFxSpec {
    fn default() -> Self {
        Self {
            kitty_sprite: "built-in CatBaker".to_string(),
            kitty_asset: crate::app_config::KittySpriteAsset::BuiltIn,
            bloom: true,
            bloom_strength: 0.35,
            bloom_radius: 2.0,
            fire_shimmer: true,
            hdr_glow: true,
            sdr_boost: 0.35,
            motion_raw: "auto".to_string(),
            motion_effective: "full".to_string(),
            motion_reason: "system permits motion".to_string(),
            adaptive_motion: true,
            performance_reduced: false,
            companion_look: aterm_effects::kitty_registry::KittyLook::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsPreviewSpec {
    pub(crate) scene: PreviewScene,
    /// Exact candidate terminal type size in logical points, independent of native-app text scale.
    pub(crate) font_px: f32,
    /// Host physical pixels per logical point; candidate bounds use terminal pixel units.
    pub(crate) terminal_scale: f32,
    /// Renderer-native typography geometry and shaping candidates.
    pub(crate) line_height: f32,
    pub(crate) baseline_adjust: i32,
    pub(crate) ligatures: bool,
    pub(crate) merged_ligatures: bool,
    pub(crate) synthetic_styles: bool,
    /// Uncommitted primary and real style-family choices. Every terminal-text
    /// primitive carries this identity through delayed rasterization.
    pub(crate) font_candidate: SemanticFontCandidate,
    pub(crate) appearance: AppearancePreviewSpec,
    /// Live OS appearance used only when `appearance.window_theme` is `auto`.
    /// It is deliberately independent of the terminal candidate palette.
    pub(crate) system_dark: bool,
    /// The window sample takes the CANDIDATE THEME's side under `auto`
    /// instead of the platform's: set by the Themes stage, whose sample
    /// shows one theme, so a light theme is never framed by a dark
    /// titlebar because the OS happens to be dark. A forced Light/Dark
    /// `window_theme` still wins.
    pub(crate) window_follows_theme: bool,
    pub(crate) typography: TypographyPreviewSpec,
    pub(crate) post_fx: CursorPostFxSpec,
    /// Exact visual field currently authoring the demo and its normalized
    /// uncommitted value. Introspection serializes this literally as `key=value`.
    pub(crate) focused_key: String,
    pub(crate) focused_value: String,
    /// The focused setting's own name, for the badge and the spoken summary.
    pub(crate) focused_label: String,
    /// The focused theme is a sample only this preview draws (a theme card
    /// pointed at or being tried), not what the configuration applies: the
    /// badge says `Sample:` instead of `Theme:`.
    pub(crate) theme_sample: bool,
    pub(crate) font_status: String,
    pub(crate) font_ready_epoch: u64,
    pub(crate) prepared_font: crate::tray_raster::PreparedSemanticFont,
    /// Candidate palette for the inner terminal only. `None` follows the host
    /// theme; either way the surrounding Settings card stays host-themed.
    pub(crate) terminal_theme: Option<PreviewTerminalTheme>,
    /// The candidate palette's label and appearance, for the accessible value.
    pub(crate) terminal_theme_identity: Option<PreviewThemeIdentity>,
    pub(crate) cursor: CursorPreviewSpec,
    /// Injected deterministic animation phase. Hosts may advance this at their
    /// own cadence; equal specs always produce equal pixels.
    pub(crate) phase_ms: u64,
    /// The already-resolved W11 motion policy. True removes all decorative
    /// motion and freezes a visible cursor instead of retaining a misleading
    /// in-flight trail.
    pub(crate) reduced_motion: bool,
    /// Why `reduced_motion` holds, for the badge and the spoken summary.
    pub(crate) pause: PreviewPause,
}

impl Default for SettingsPreviewSpec {
    fn default() -> Self {
        Self {
            scene: PreviewScene::CursorMotion,
            font_px: 14.0,
            terminal_scale: 1.0,
            line_height: 1.0,
            baseline_adjust: 0,
            ligatures: true,
            merged_ligatures: false,
            synthetic_styles: true,
            font_candidate: SemanticFontCandidate::default(),
            appearance: AppearancePreviewSpec::default(),
            system_dark: false,
            window_follows_theme: false,
            typography: TypographyPreviewSpec::default(),
            post_fx: CursorPostFxSpec::default(),
            focused_key: "cursor_trail_style".to_string(),
            focused_value: crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.to_string(),
            focused_label: String::new(),
            theme_sample: false,
            font_status: "host-prepared renderer snapshot unavailable".to_string(),
            font_ready_epoch: 0,
            prepared_font: crate::tray_raster::PreparedSemanticFont::unavailable(
                SemanticFontCandidate::default(),
            ),
            terminal_theme: None,
            terminal_theme_identity: None,
            cursor: CursorPreviewSpec::default(),
            phase_ms: 720,
            reduced_motion: false,
            pause: PreviewPause::Unspecified,
        }
    }
}

impl SettingsPreviewSpec {
    pub(crate) fn appearance(font_px: f32) -> Self {
        Self {
            scene: PreviewScene::Appearance,
            font_px,
            cursor: CursorPreviewSpec {
                trail_enabled: false,
                blink: false,
                ..CursorPreviewSpec::default()
            },
            ..Self::default()
        }
    }

    pub(crate) fn typography(font_px: f32) -> Self {
        Self {
            scene: PreviewScene::Typography,
            font_px,
            cursor: CursorPreviewSpec {
                trail_enabled: false,
                blink: false,
                ..CursorPreviewSpec::default()
            },
            ..Self::default()
        }
    }

    pub(crate) fn cursor(cursor: CursorPreviewSpec) -> Self {
        Self {
            cursor,
            ..Self::default()
        }
    }

    pub(crate) fn with_phase(mut self, phase_ms: u64) -> Self {
        self.phase_ms = phase_ms;
        self
    }

    pub(crate) fn with_reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.reduced_motion = reduced_motion;
        self
    }

    pub(crate) fn with_pause(mut self, pause: PreviewPause) -> Self {
        self.pause = pause;
        self
    }

    pub(crate) fn with_terminal_theme(mut self, terminal_theme: PreviewTerminalTheme) -> Self {
        self.terminal_theme = Some(terminal_theme);
        self
    }

    pub(crate) fn with_terminal_theme_identity(mut self, identity: PreviewThemeIdentity) -> Self {
        self.terminal_theme_identity = Some(identity);
        self
    }

    pub(crate) fn with_typography(
        mut self,
        line_height: f32,
        baseline_adjust: i32,
        ligatures: bool,
        merged_ligatures: bool,
        synthetic_styles: bool,
    ) -> Self {
        self.line_height = line_height;
        self.baseline_adjust = baseline_adjust;
        self.ligatures = ligatures;
        self.merged_ligatures = merged_ligatures;
        self.synthetic_styles = synthetic_styles;
        self
    }

    pub(crate) fn with_font_candidate(mut self, candidate: SemanticFontCandidate) -> Self {
        self.font_candidate = candidate;
        self
    }

    pub(crate) fn with_focus(
        mut self,
        key: impl Into<String>,
        normalized_value: impl Into<String>,
    ) -> Self {
        self.focused_key = key.into();
        self.focused_value = normalized_value.into();
        self
    }

    pub(crate) fn with_focus_label(mut self, label: impl Into<String>) -> Self {
        self.focused_label = label.into();
        self
    }

    /// Mark the focused theme as a sample this preview alone draws.
    pub(crate) fn with_theme_sample(mut self, sample: bool) -> Self {
        self.theme_sample = sample;
        self
    }

    pub(crate) fn with_prepared_font(
        mut self,
        prepared: crate::tray_raster::PreparedSemanticFont,
    ) -> Self {
        self.font_status = prepared.snapshot.status.clone();
        self.font_ready_epoch = prepared.snapshot.ready_epoch;
        self.prepared_font = prepared;
        self
    }

    pub(crate) fn with_appearance(mut self, appearance: AppearancePreviewSpec) -> Self {
        self.appearance = appearance;
        self
    }

    pub(crate) fn with_system_dark(mut self, system_dark: bool) -> Self {
        self.system_dark = system_dark;
        self
    }

    pub(crate) fn with_window_following_theme(mut self, follows: bool) -> Self {
        self.window_follows_theme = follows;
        self
    }

    pub(crate) fn with_typography_candidate(mut self, typography: TypographyPreviewSpec) -> Self {
        self.typography = typography;
        self
    }

    pub(crate) fn with_post_fx(mut self, post_fx: CursorPostFxSpec) -> Self {
        self.post_fx = post_fx;
        self
    }

    fn normalized_font_px(&self) -> f32 {
        let scale = finite_or(self.terminal_scale, 1.0).max(f32::EPSILON);
        finite_or(self.font_px, 14.0 / scale)
            .clamp(crate::FONT_PX_MIN / scale, crate::FONT_PX_MAX / scale)
    }

    fn normalized(&self) -> Self {
        Self {
            scene: self.scene,
            font_px: self.normalized_font_px(),
            terminal_scale: finite_or(self.terminal_scale, 1.0).max(f32::EPSILON),
            line_height: finite_or(self.line_height, 1.0).clamp(0.8, 2.0),
            baseline_adjust: self.baseline_adjust.clamp(-32, 32),
            ligatures: self.ligatures,
            merged_ligatures: self.merged_ligatures,
            synthetic_styles: self.synthetic_styles,
            font_candidate: self.font_candidate.clone(),
            appearance: AppearancePreviewSpec {
                window_theme: self.appearance.window_theme.trim().to_ascii_lowercase(),
                minimum_contrast: finite_or(self.appearance.minimum_contrast, 1.0).clamp(1.0, 21.0),
                selection_foreground: self
                    .appearance
                    .selection_foreground
                    .map(|color| color & 0x00ff_ffff),
                selection_inactive: self.appearance.selection_inactive,
                bold_is_bright: self.appearance.bold_is_bright,
                faint_opacity: finite_or(self.appearance.faint_opacity, 0.5).clamp(0.0, 1.0),
            },
            system_dark: self.system_dark,
            window_follows_theme: self.window_follows_theme,
            typography: TypographyPreviewSpec {
                cursor_break_ligatures: self.typography.cursor_break_ligatures,
                underline_position: self.typography.underline_position.clamp(-32, 32),
                underline_thickness: self.typography.underline_thickness.clamp(-32, 32),
                underline_skip_descenders: self.typography.underline_skip_descenders,
                text_blending: self.typography.text_blending,
                font_thicken: self.typography.font_thicken,
                stem_gamma: finite_or(self.typography.stem_gamma, 1.0).clamp(0.3, 3.0),
                variations: self.typography.variations.clone(),
            },
            post_fx: CursorPostFxSpec {
                kitty_sprite: self.post_fx.kitty_sprite.trim().to_string(),
                kitty_asset: self.post_fx.kitty_asset.clone(),
                bloom: self.post_fx.bloom,
                bloom_strength: finite_or(self.post_fx.bloom_strength, 0.35).clamp(0.0, 3.0),
                bloom_radius: finite_or(self.post_fx.bloom_radius, 2.0).clamp(0.5, 8.0),
                fire_shimmer: self.post_fx.fire_shimmer,
                hdr_glow: self.post_fx.hdr_glow,
                sdr_boost: finite_or(self.post_fx.sdr_boost, 0.35).clamp(0.0, 1.0),
                motion_raw: self.post_fx.motion_raw.trim().to_ascii_lowercase(),
                motion_effective: self.post_fx.motion_effective.clone(),
                motion_reason: self.post_fx.motion_reason.clone(),
                adaptive_motion: self.post_fx.adaptive_motion,
                performance_reduced: self.post_fx.performance_reduced,
                companion_look: self.post_fx.companion_look,
            },
            focused_key: self.focused_key.trim().to_string(),
            focused_value: self.focused_value.trim().to_string(),
            focused_label: self.focused_label.trim().to_string(),
            theme_sample: self.theme_sample,
            font_status: self.font_status.clone(),
            font_ready_epoch: self.font_ready_epoch,
            prepared_font: self.prepared_font.clone(),
            terminal_theme: self.terminal_theme.map(PreviewTerminalTheme::normalized),
            terminal_theme_identity: self.terminal_theme_identity.clone(),
            cursor: self.cursor.normalized(),
            phase_ms: self.phase_ms % PHASE_CYCLE_MS,
            reduced_motion: self.reduced_motion,
            pause: self.pause,
        }
    }

    /// Return the exact cadence required by the pixels this spec can paint.
    /// Static, hidden, reduced-motion, and zero-intensity combinations return
    /// `None`, preserving aterm's pure-Wait idle contract.
    ///
    /// Answered from the spec as it stands, normalized or not, without
    /// materializing `normalized()`: the event loop probes the cadence for every
    /// window on every `about_to_wait` turn, and the retained-raster key needs it
    /// once per presented frame, so neither pays the ~10 heap allocations
    /// `normalized()` costs to produce a three-variant enum. Nothing it reads
    /// needs normalizing except a non-finite `intensity`, which collapses to 0.0
    /// (no motion) here exactly as it does in paint;
    /// `millis_until_next_blink_edge` re-applies `% PHASE_CYCLE_MS` itself.
    pub(crate) fn animation(&self) -> PreviewAnimation {
        if self.reduced_motion {
            return PreviewAnimation::None;
        }
        self.animation_without_reduction()
    }

    fn animation_without_reduction(&self) -> PreviewAnimation {
        // Appearance and Typography never paint the cursor-motion lane below.
        // Their shared candidate state still carries cursor fields so switching
        // scenes is lossless, but invisible trail/blink values must not arm the
        // event-loop scheduler or enter retained-paint identity.
        if self.scene != PreviewScene::CursorMotion {
            return PreviewAnimation::None;
        }
        // The scripted typing plays wherever typing would light something in
        // a window: a trail, or the momentum glow, which obeys only the
        // effects switch and its own (Off keeps it, at any intensity). A
        // trail pack that cannot load fails closed: the page says so over a
        // still frame.
        if self.cursor.trail_enabled
            && !self.cursor.trail_pack_unavailable()
            && (self.cursor.momentum_glow
                || (self.cursor.has_resolved_trail()
                    && finite_or(self.cursor.intensity, 0.0) > 0.0))
        {
            return PreviewAnimation::Continuous;
        }
        if self.cursor.blink {
            return PreviewAnimation::BlinkEdge {
                after_ms: self.millis_until_next_blink_edge(),
            };
        }
        PreviewAnimation::None
    }

    fn reduction_suppresses_motion(&self) -> bool {
        self.reduced_motion && self.animation_without_reduction() != PreviewAnimation::None
    }

    fn cursor_visible(&self) -> bool {
        self.reduced_motion
            || !self.cursor.blink
            || (self.phase_ms / CURSOR_BLINK_HALF_MS).is_multiple_of(2)
    }

    /// A literal in-specimen acknowledgement for font assets and axes, only
    /// where the glyphs cannot speak for themselves: while the font loads, or
    /// when it failed and the current face is still being shown. A font that
    /// is ready has none; the header badge already names it, and the band
    /// goes to sample rows. It is deliberately inside the terminal canvas,
    /// not the generic candidate badge above it.
    fn font_candidate_pill(&self) -> Option<(String, bool)> {
        let label = match self.focused_key.as_str() {
            crate::prefs::EDIT_FONT_FAMILY => "Regular",
            crate::prefs::EDIT_FONT_FAMILY_BOLD => "Bold",
            crate::prefs::EDIT_FONT_FAMILY_ITALIC => "Italic",
            crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC => "Bold Italic",
            crate::prefs::EDIT_FALLBACK_FONTS => "Fallback",
            crate::prefs::EDIT_SYMBOL_FONT => "Symbols",
            crate::prefs::EDIT_EMOJI_FONT => "Emoji",
            crate::prefs::EDIT_FONT_WEIGHT => "Weight",
            crate::prefs::EDIT_FONT_VARIATION => "Axes",
            _ => return None,
        };
        let status = self.font_status.to_ascii_lowercase();
        // Everything past the loading arm is a warning: the font failed.
        let state = if status.contains("unresolved") {
            // A name no resolver found, in the words its row uses.
            "Not found"
        } else if status.contains("unavailable") || status.contains("fallback active") {
            // Keep the authored family/axis identity visible inside the bounded
            // capsule. The surrounding specimen already demonstrates the
            // current fallback face; repeating that fact here used the width
            // budget that belongs to the candidate the user is editing.
            "Unavailable"
        } else if self.prepared_font.snapshot.pending
            || status.contains("loading")
            || status.contains("queued")
            || status.contains("ready for host preparation")
        {
            // The header names the font being loaded; the pill only says
            // the glyphs below are not it yet.
            return Some(("Loading font".to_string(), false));
        } else {
            return None;
        };
        // A font that failed is named by its own slot, whichever row has
        // focus: an emoji font that is not installed says Emoji.
        let candidate = &self.font_candidate;
        let failed =
            crate::tray_raster::first_unresolved_slot(&self.font_status).and_then(|slot| {
                let (label, family) = match slot {
                    "regular" => ("Regular", candidate.regular.as_deref()),
                    "bold" => ("Bold", candidate.bold.as_deref()),
                    "italic" => ("Italic", candidate.italic.as_deref()),
                    "bold italic" => ("Bold Italic", candidate.bold_italic.as_deref()),
                    "symbol" => ("Symbols", candidate.symbol.as_deref()),
                    "emoji" => ("Emoji", candidate.emoji.as_deref()),
                    _ => ("Fallback", candidate.fallback.first().map(String::as_str)),
                };
                Some((label, family_words(family?)))
            });
        if let Some((label, family)) = failed {
            return Some((format!("{state} · {label}: {family}"), true));
        }
        // The same words as the header badge and the row's own control.
        let value = match self.focused_font_slot() {
            Some(Some(family)) => family_words(family),
            Some(None) => self.automatic_words(),
            None if self.focused_value.is_empty() => "system default".to_string(),
            None => self.focused_value.clone(),
        };
        Some((format!("{state} · {label}: {value}"), true))
    }

    /// `phase_ms` is cyclic, while 2,400 ms is intentionally not an integer
    /// multiple of the 530 ms blink half-period. The cycle reset occurs during
    /// an already-visible half, so it is not itself a visual edge; after the
    /// final in-cycle edge the next wake is 530 ms into the next cycle.
    fn millis_until_next_blink_edge(&self) -> u64 {
        let phase = self.phase_ms % PHASE_CYCLE_MS;
        for multiple in 1.. {
            let edge = multiple * CURSOR_BLINK_HALF_MS;
            if edge >= PHASE_CYCLE_MS {
                break;
            }
            if edge > phase {
                return edge - phase;
            }
        }
        PHASE_CYCLE_MS - phase + CURSOR_BLINK_HALF_MS
    }

    pub(crate) fn semantic_label(&self) -> String {
        self.scene.label().to_string()
    }

    pub(crate) fn semantic_value(&self) -> String {
        let spec = self.normalized();
        match spec.scene {
            PreviewScene::Appearance => {
                // The theme focused is named in the badge's words ("Follow
                // system · Default", a sample's name), so the two never
                // disagree about which theme is drawn.
                let identity = match spec.candidate_parts() {
                    Some((_, value)) if spec.focused_key == crate::prefs::EDIT_THEME => spec
                        .terminal_theme_identity
                        .as_ref()
                        .map(|identity| PreviewThemeIdentity {
                            label: value,
                            light: identity.light,
                        }),
                    _ => spec.terminal_theme_identity.clone(),
                };
                let colors = spec.terminal_theme.map_or_else(
                    || "the current terminal colors".to_string(),
                    |theme| spoken_palette(theme, identity.as_ref()),
                );
                let window = match spec.appearance.window_theme.as_str() {
                    "light" => "Light window appearance",
                    "dark" => "Dark window appearance",
                    // Linux Auto follows the TERMINAL THEME, not a live OS
                    // appearance feed (`chrome_theme_for_apprt`); announce the
                    // side the painter resolves through `window_sample_light`,
                    // never a second opinion of our own.
                    //
                    // With no candidate palette the painter falls back to the
                    // HOST terminal theme — a colour the semantic projection is
                    // compiled long before any theme is bound to, so it names
                    // the RULE and no side, rather than substituting the
                    // `system_dark` OS feed this platform's Auto explicitly does
                    // not consult. (Every shipping spec carries a candidate:
                    // `renderer_preview_spec_for_key_with_font` derives one from
                    // the host theme even when nothing is edited.)
                    _ if spec.window_follows_theme || cfg!(target_os = "linux") => {
                        match spec.terminal_theme {
                            Some(candidate) => {
                                if window_sample_light(&spec, candidate.into_theme()) {
                                    "window appearance that matches the terminal theme, currently Light"
                                } else {
                                    "window appearance that matches the terminal theme, currently Dark"
                                }
                            }
                            None => "window appearance that matches the terminal theme",
                        }
                    }
                    _ if spec.system_dark && cfg!(target_os = "macos") => {
                        "window appearance that follows macOS, currently Dark"
                    }
                    _ if cfg!(target_os = "macos") => {
                        "window appearance that follows macOS, currently Light"
                    }
                    _ if spec.system_dark => {
                        "window appearance that follows the system, currently Dark"
                    }
                    _ => "window appearance that follows the system, currently Light",
                };
                // "Sample" only when the stage draws a card, never for the
                // theme the configuration applies.
                let subject = if spec.theme_sample {
                    "Color theme sample"
                } else {
                    "Color theme"
                };
                format!("{subject}: {colors}, with {window}.")
            }
            PreviewScene::Typography => {
                // A family no resolver found is not what the sample shows.
                let regular_missing = spec.font_status.to_ascii_lowercase().contains("unresolved")
                    && crate::tray_raster::first_unresolved_slot(&spec.font_status)
                        == Some("regular");
                let family = match spec
                    .font_candidate
                    .regular
                    .as_deref()
                    .map(str::trim)
                    .filter(|family| !family.is_empty())
                {
                    Some(family) if regular_missing => format!(
                        "the current font, because \u{201c}{}\u{201d} is not found",
                        family_words(family)
                    ),
                    Some(family) => family_words(family),
                    None => "the current font".to_string(),
                };
                let detail = spec
                    .typography_detail()
                    .map_or_else(String::new, |detail| format!(", {detail}"));
                format!(
                    "Text sample in {family} at {} pixels{detail}; ligatures are {} and synthetic styles are {}.",
                    trim_float(spec.font_px * spec.terminal_scale),
                    if spec.ligatures { "on" } else { "off" },
                    if spec.synthetic_styles { "on" } else { "off" },
                )
            }
            PreviewScene::CursorMotion => {
                if spec.cursor.trail_pack_unavailable() {
                    return "Cursor preview unavailable because the selected trail could not be loaded."
                        .to_string();
                }
                let trail = if spec.cursor.trail_enabled && spec.cursor.has_resolved_trail() {
                    format!("{} trail", spec.cursor.trail_style_label())
                } else if spec.cursor.trail_enabled && spec.cursor.momentum_glow {
                    match spec.typing_glow() {
                        TypingGlow::Halo => "no trail, glowing as you type",
                        TypingGlow::Tint => "no trail, tinting as you type",
                        TypingGlow::Nothing => "no trail",
                    }
                    .to_string()
                } else {
                    "no trail".to_string()
                };
                let state = if spec.reduction_suppresses_motion() {
                    format!(", paused because {}", spec.pause.because())
                } else {
                    match spec.animation() {
                        PreviewAnimation::Continuous => ", shown while typing".to_string(),
                        PreviewAnimation::BlinkEdge { .. } => ", blinking".to_string(),
                        PreviewAnimation::None => String::new(),
                    }
                };
                format!(
                    "Cursor preview: {} cursor, {trail}{state}.",
                    spec.cursor.style.label()
                )
            }
        }
    }

    /// An unset font row's words: the "Automatic (SF Mono)" its control
    /// shows where Settings named the face, else "Automatic".
    fn automatic_words(&self) -> String {
        if self.focused_value.starts_with("Automatic") {
            self.focused_value.clone()
        } else {
            "Automatic".to_string()
        }
    }

    /// The focused font row's slot, when the specimen's font status says no
    /// resolver found its family.
    fn unresolved_focused_slot(&self) -> Option<&'static str> {
        let slot = match self.focused_key.as_str() {
            crate::prefs::EDIT_FONT_FAMILY => "regular",
            crate::prefs::EDIT_FONT_FAMILY_BOLD => "bold",
            crate::prefs::EDIT_FONT_FAMILY_ITALIC => "italic",
            crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC => "bold italic",
            crate::prefs::EDIT_SYMBOL_FONT => "symbol",
            crate::prefs::EDIT_EMOJI_FONT => "emoji",
            _ => return None,
        };
        (crate::tray_raster::first_unresolved_slot(&self.font_status) == Some(slot)).then_some(slot)
    }

    /// The family a focused font row has chosen, as the specimen draws it:
    /// `Some(None)` when that row is left to the default or to discovery, and
    /// `None` when the focus is not a font row.
    fn focused_font_slot(&self) -> Option<Option<&str>> {
        let candidate = &self.font_candidate;
        let family = match self.focused_key.as_str() {
            crate::prefs::EDIT_FONT_FAMILY => candidate.regular.as_deref(),
            crate::prefs::EDIT_FONT_FAMILY_BOLD => candidate.bold.as_deref(),
            crate::prefs::EDIT_FONT_FAMILY_ITALIC => candidate.italic.as_deref(),
            crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC => candidate.bold_italic.as_deref(),
            crate::prefs::EDIT_SYMBOL_FONT => candidate.symbol.as_deref(),
            crate::prefs::EDIT_EMOJI_FONT => candidate.emoji.as_deref(),
            crate::prefs::EDIT_FALLBACK_FONTS => {
                (!candidate.fallback.is_empty()).then_some(self.focused_value.as_str())
            }
            _ => return None,
        };
        Some(family.map(str::trim).filter(|family| !family.is_empty()))
    }

    /// The focused text setting in words, when the sentence does not already
    /// say it: `line height 1.6×`, `thicken off`, `emoji font automatic`.
    fn typography_detail(&self) -> Option<String> {
        if crate::prefs::section_of(&self.focused_key) != crate::prefs::Section::Typography
            || matches!(
                self.focused_key.as_str(),
                crate::prefs::EDIT_FONT_FAMILY
                    | crate::prefs::EDIT_FONT_PX
                    | crate::prefs::EDIT_LIGATURES
                    | crate::prefs::EDIT_FONT_SYNTHETIC_STYLE
            )
        {
            return None;
        }
        let (subject, value) = self.candidate_parts()?;
        let value = if matches!(value.as_str(), "On" | "Off" | "Automatic") {
            value.to_lowercase()
        } else {
            value
        };
        Some(format!("{} {value}", subject.to_lowercase()))
    }

    /// Detailed renderer audit projection for introspection and regression
    /// oracles. This deliberately does not back the accessibility value: terms
    /// such as internal primitive names, fingerprints, and tone-map details are
    /// useful to developers but punishing when spoken by assistive technology.
    pub(crate) fn audit_value(&self) -> String {
        let spec = self.normalized();
        let motion = match spec.animation() {
            PreviewAnimation::None if spec.reduction_suppresses_motion() => {
                "reduced motion; static"
            }
            PreviewAnimation::None => "static",
            PreviewAnimation::BlinkEdge { .. } => "live cursor blink",
            PreviewAnimation::Continuous => "live motion",
        };
        let terminal_theme = spec.terminal_theme.map_or_else(
            || "host terminal theme".to_string(),
            PreviewTerminalTheme::semantic_description,
        );
        let trail_emitted = spec.cursor.trail_enabled
            && spec.cursor.has_resolved_trail()
            && spec.cursor.intensity > 0.0;
        let trail_scope = if spec.cursor.trail_pack_unavailable() {
            "The selected Trail Pack is unavailable; the preview fails closed and emits no CursorGlow geometry."
        } else if trail_emitted {
            if spec.cursor.trail_style == PreviewTrailStyle::Custom {
                "The shared CursorGlow custom Trail Pack interpreter is live for the selected pack."
            } else if let PreviewTrailCompanion::Pet(species) = spec.cursor.trail_companion() {
                match species {
                    aterm_effects::kitty_pet::PetSpecies::Cat => {
                        "Shared CursorGlow rainbow geometry and the resident cat companion are live."
                    }
                    aterm_effects::kitty_pet::PetSpecies::Dog => {
                        "Shared CursorGlow rainbow geometry and the resident dog companion are live."
                    }
                }
            } else if spec.cursor.trail_companion() == PreviewTrailCompanion::FlyingKitty {
                "Shared CursorGlow rainbow geometry and the CatBaker flying-kitty sprite are live."
            } else {
                "Shared CursorGlow trail geometry is live for the selected effect."
            }
        } else {
            "No CursorGlow trail primitive is emitted for this scene."
        };
        let candidate = format!("{}={}", spec.focused_key, spec.focused_value);
        let selection_fg = spec.appearance.selection_foreground.map_or_else(
            || "auto contrast floor".to_string(),
            |color| format!("#{color:06X}"),
        );
        let post_fx = format!(
            "portable SDR tone-map: bloom {} strength {} radius {}; fire shimmer {}; HDR candidate {} tone-mapped (no panel-headroom claim); SDR boost {}",
            if spec.post_fx.bloom { "on" } else { "off" },
            trim_float(spec.post_fx.bloom_strength),
            trim_float(spec.post_fx.bloom_radius),
            if spec.post_fx.fire_shimmer {
                "on"
            } else {
                "off"
            },
            if spec.post_fx.hdr_glow { "on" } else { "off" },
            trim_float(spec.post_fx.sdr_boost),
        );
        let kitty_asset = match &spec.post_fx.kitty_asset {
            crate::app_config::KittySpriteAsset::BuiltIn => {
                "built-in CatBaker asset ready".to_string()
            }
            crate::app_config::KittySpriteAsset::Ready {
                source_id,
                w,
                h,
                fp,
                ..
            } => format!("resolved custom asset {source_id:?}, {w}×{h}, fp {fp:016x}"),
            crate::app_config::KittySpriteAsset::Invalid {
                source_id,
                bounded_reason,
            } => format!("custom asset {source_id:?} disabled: {bounded_reason}"),
        };
        let kitty_activation = if spec.focused_key == crate::prefs::EDIT_CURSOR_NYAN_SPRITE
            && spec.cursor.trail_companion() != PreviewTrailCompanion::FlyingKitty
        {
            "The chosen sprite is shown independently in this specimen; it remains dormant in terminal sessions until trail style Rainbow kitty flying is selected."
        } else {
            "The sprite follows the selected terminal trail style."
        };
        let focus_lane = if spec.focused_key == crate::prefs::EDIT_CURSOR_FIRE_SHIMMER {
            "Focused Fire-shimmer control uses a bounded Fire runway; the authored trail-style setting is unchanged."
        } else if spec.focused_key == crate::prefs::EDIT_LOAD_ADAPTIVE_MOTION {
            "Focused adaptive-motion control uses a bounded representative load signal; live performance state is unchanged."
        } else {
            "The specimen follows the authored candidate directly."
        };
        let font_pill = spec.font_candidate_pill().map_or_else(
            || "No font-status capsule is needed for this control.".to_string(),
            |(label, _)| format!("In-specimen font status: {label}."),
        );
        format!(
            "normalized-candidate {candidate}; renderer preview: {} px monospace, {}× line height, baseline {:+} px; ligatures {}, cursor-break ligatures {}, merged ligatures {}, synthetic styles {}; {}; {font_pill} {terminal_theme}; selection foreground {selection_fg}, minimum contrast {}, selection {}; underline position {:+} thickness {:+} skip descenders {}; text blending {:?}, font thicken {}, stem gamma {}, variation requests {}; {} cursor; blink {}; {} trail; raw/effective motion {}/{} because {}; adaptive motion {} load-shed {}; {motion}. {trail_scope} {post_fx}; Rainbow kitty sprite {:?}.",
            trim_float(spec.font_px * spec.terminal_scale),
            trim_float(spec.line_height),
            spec.baseline_adjust,
            if spec.ligatures { "on" } else { "off" },
            if spec.typography.cursor_break_ligatures {
                "on"
            } else {
                "off"
            },
            if spec.merged_ligatures { "on" } else { "off" },
            if spec.synthetic_styles { "on" } else { "off" },
            spec.font_status,
            trim_float(spec.appearance.minimum_contrast),
            if spec.appearance.selection_inactive {
                "inactive"
            } else {
                "focused"
            },
            spec.typography.underline_position,
            spec.typography.underline_thickness,
            if spec.typography.underline_skip_descenders {
                "on"
            } else {
                "off"
            },
            spec.typography.text_blending,
            if spec.typography.font_thicken {
                "on"
            } else {
                "off"
            },
            trim_float(spec.typography.stem_gamma),
            spec.typography.variations.len(),
            spec.cursor.style.label(),
            if spec.cursor.blink { "on" } else { "off" },
            spec.cursor.effective_trail_style_raw(),
            spec.post_fx.motion_raw,
            spec.post_fx.motion_effective,
            spec.post_fx.motion_reason,
            if spec.post_fx.adaptive_motion {
                "on"
            } else {
                "off"
            },
            if spec.post_fx.performance_reduced {
                "active"
            } else {
                "healthy"
            },
            format_args!(
                "{:?}; {kitty_asset}. {kitty_activation} {focus_lane}",
                spec.post_fx.kitty_sprite
            ),
        )
    }

    /// Paint-only identity folded into [`crate::native_ui::CompiledUi`]'s
    /// retained-raster key. Animation phase deliberately stays out of the
    /// accessibility value, but it must invalidate pixels while motion runs.
    /// Under reduced motion phase cannot affect paint and is normalized to 0,
    /// preventing a frozen preview from needlessly re-rasterizing.
    pub(crate) fn paint_fingerprint(&self) -> u64 {
        let fingerprint = self.paint_identity_hash();
        // Drift guard for the invariant documented on `paint_identity_hash`:
        // hashing a spec and hashing its normalization must agree. Any raw
        // field that leaks into the key (a missing clamp, trim, mask, or the
        // `phase_ms` modulo) breaks this equality for un-normalized input, so
        // every debug-built test that touches a fingerprint pins it.
        debug_assert_eq!(
            fingerprint,
            self.normalized().paint_identity_hash(),
            "paint identity must hash normalized values only"
        );
        fingerprint
    }

    /// The hashing half of [`Self::paint_fingerprint`].
    ///
    /// This runs on the present path (once per native frame, once per visible
    /// native leaf on the split route), so it hashes each field's NORMALIZED
    /// projection in place instead of materializing a whole `normalized()`
    /// clone to hash — the same "touches NO heap" posture
    /// [`crate::native_ui::CompiledUi::fingerprint`] documents for itself, with
    /// FxHash over SipHash for the same stated reason: a cache key over
    /// first-party data, never persisted and never sent on the wire.
    ///
    /// INVARIANT: the hashed value SET and its many-to-one collapses must stay
    /// exactly those of `normalized()` — same clamps, same `0x00ff_ffff` masks,
    /// same `trail_pack`-only-for-`Custom` rule, and the same
    /// `phase_ms % PHASE_CYCLE_MS`. Hashing coarser would stale-serve a
    /// retained raster; hashing finer would re-raster identical pixels.
    fn paint_identity_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = aterm_hash::FxHasher::default();
        (self.scene as u8).hash(&mut hash);
        self.normalized_font_px().to_bits().hash(&mut hash);
        finite_or(self.line_height, 1.0)
            .clamp(0.8, 2.0)
            .to_bits()
            .hash(&mut hash);
        self.baseline_adjust.clamp(-32, 32).hash(&mut hash);
        self.ligatures.hash(&mut hash);
        self.merged_ligatures.hash(&mut hash);
        self.synthetic_styles.hash(&mut hash);
        self.font_candidate.hash(&mut hash);
        self.font_ready_epoch.hash(&mut hash);
        self.focused_key.trim().hash(&mut hash);
        self.focused_value.trim().hash(&mut hash);
        hash_trimmed_lowercase(&self.appearance.window_theme, &mut hash);
        finite_or(self.appearance.minimum_contrast, 1.0)
            .clamp(1.0, 21.0)
            .to_bits()
            .hash(&mut hash);
        self.appearance
            .selection_foreground
            .map(|color| color & 0x00ff_ffff)
            .hash(&mut hash);
        self.appearance.selection_inactive.hash(&mut hash);
        self.appearance.bold_is_bright.hash(&mut hash);
        finite_or(self.appearance.faint_opacity, 0.5)
            .clamp(0.0, 1.0)
            .to_bits()
            .hash(&mut hash);
        // Compare the NORMALIZED theme word: an authored " Auto " must still
        // fold `system_dark` in, or an OS light/dark flip would not invalidate
        // the raster.
        if self
            .appearance
            .window_theme
            .trim()
            .eq_ignore_ascii_case("auto")
        {
            self.system_dark.hash(&mut hash);
            self.window_follows_theme.hash(&mut hash);
        }
        self.typography.cursor_break_ligatures.hash(&mut hash);
        self.typography
            .underline_position
            .clamp(-32, 32)
            .hash(&mut hash);
        self.typography
            .underline_thickness
            .clamp(-32, 32)
            .hash(&mut hash);
        self.typography.underline_skip_descenders.hash(&mut hash);
        (self.typography.text_blending as u8).hash(&mut hash);
        self.typography.font_thicken.hash(&mut hash);
        finite_or(self.typography.stem_gamma, 1.0)
            .clamp(0.3, 3.0)
            .to_bits()
            .hash(&mut hash);
        self.typography.variations.hash(&mut hash);
        self.post_fx.kitty_sprite.trim().hash(&mut hash);
        self.post_fx.kitty_asset.fingerprint().hash(&mut hash);
        let look = self.post_fx.companion_look;
        (look.variant, look.accessory, look.coat, look.iris).hash(&mut hash);
        self.post_fx.bloom.hash(&mut hash);
        finite_or(self.post_fx.bloom_strength, 0.35)
            .clamp(0.0, 3.0)
            .to_bits()
            .hash(&mut hash);
        finite_or(self.post_fx.bloom_radius, 2.0)
            .clamp(0.5, 8.0)
            .to_bits()
            .hash(&mut hash);
        self.post_fx.fire_shimmer.hash(&mut hash);
        self.post_fx.hdr_glow.hash(&mut hash);
        finite_or(self.post_fx.sdr_boost, 0.35)
            .clamp(0.0, 1.0)
            .to_bits()
            .hash(&mut hash);
        hash_trimmed_lowercase(&self.post_fx.motion_raw, &mut hash);
        self.post_fx.motion_effective.hash(&mut hash);
        self.post_fx.motion_reason.hash(&mut hash);
        self.post_fx.adaptive_motion.hash(&mut hash);
        self.post_fx.performance_reduced.hash(&mut hash);
        self.focused_label.trim().hash(&mut hash);
        self.theme_sample.hash(&mut hash);
        // `PreviewTerminalTheme` is `Copy`, so the normalized candidate is a
        // stack value.
        self.terminal_theme
            .map(PreviewTerminalTheme::normalized)
            .hash(&mut hash);
        (self.cursor.style as u8).hash(&mut hash);
        self.cursor.blink.hash(&mut hash);
        self.cursor.trail_enabled.hash(&mut hash);
        (self.cursor.trail_style as u8).hash(&mut hash);
        self.cursor
            .effective_trail_style_raw()
            .trim()
            .hash(&mut hash);
        // Pack identity counts only where the engine can dispatch to it, so two
        // built-ins carrying a stale pack stay one key, exactly as today.
        let trail_pack_fp = if matches!(self.cursor.trail_style, PreviewTrailStyle::Custom) {
            self.cursor.trail_pack.as_ref().map(|pack| pack.pack_fp)
        } else {
            None
        };
        trail_pack_fp.hash(&mut hash);
        self.cursor.normalized().trail_pack_name.hash(&mut hash);
        self.cursor
            .color
            .map(|color| color & 0x00ff_ffff)
            .hash(&mut hash);
        self.cursor
            .accent
            .map(|color| color & 0x00ff_ffff)
            .hash(&mut hash);
        self.cursor
            .duration_ms
            .clamp(MIN_DURATION_MS, MAX_DURATION_MS)
            .hash(&mut hash);
        self.cursor.length.clamp(1, MAX_LENGTH).hash(&mut hash);
        finite_or(self.cursor.intensity, 0.0)
            .clamp(0.0, 1.0)
            .to_bits()
            .hash(&mut hash);
        finite_or(self.cursor.radius, 0.0)
            .clamp(0.0, 2.0)
            .to_bits()
            .hash(&mut hash);
        self.cursor.ring.hash(&mut hash);
        self.cursor.momentum_glow.hash(&mut hash);
        self.cursor.cursor_color_pinned.hash(&mut hash);
        self.reduced_motion.hash(&mut hash);
        self.pause.hash(&mut hash);
        // The phase MUST be reduced before the blink parity is read: 2,400 ms is
        // deliberately not a whole multiple of the 530 ms half-period, so raw
        // phase 4,800 (parity odd) and normalized phase 0 (parity even) disagree.
        // Paint goes through `normalized()`, so the key must too, or two frames
        // that paint differently collide on one retained raster.
        let phase_ms = self.phase_ms % PHASE_CYCLE_MS;
        let cursor_visible = self.reduced_motion
            || !self.cursor.blink
            || (phase_ms / CURSOR_BLINK_HALF_MS).is_multiple_of(2);
        match self.animation() {
            PreviewAnimation::Continuous => phase_ms.hash(&mut hash),
            PreviewAnimation::BlinkEdge { .. } => cursor_visible.hash(&mut hash),
            PreviewAnimation::None => 0_u64.hash(&mut hash),
        }
        hash.finish() | 1
    }

    /// Typography keeps the candidate's exact size. Buy the five specimen rows
    /// and a separate font-status band instead of painting a badge over glyphs.
    pub(crate) fn preferred_height(&self, minimum: f32) -> f32 {
        if self.scene != PreviewScene::Typography {
            return minimum;
        }
        minimum.max(self.typography_height(TYPOGRAPHY_SPECIMEN_ROWS))
    }

    /// The least height this preview still demonstrates at. Every scene paints
    /// whole rows at the authored size, never a shrunken one: a typography
    /// specimen keeps one row and its status band; a cursor specimen keeps one
    /// row for the prompt its cursor types on; a color specimen keeps one row
    /// above its palette swatches. A host that cannot give this height shows
    /// no preview rather than a sliver ([`Self::paint`] draws only the header
    /// below it).
    pub(crate) fn minimum_height(&self, floor: f32) -> f32 {
        let spec = self.normalized();
        floor.max(match self.scene {
            PreviewScene::Typography => self.typography_height(1),
            PreviewScene::CursorMotion => {
                preview_header_height(&spec)
                    + 15.0
                    + spec.specimen_headroom()
                    + spec.typography_row_height()
            }
            PreviewScene::Appearance => {
                preview_header_height(&spec)
                    + 15.0
                    + appearance_terminal_floor(
                        spec.specimen_headroom() + spec.typography_row_height(),
                    )
            }
        })
    }

    /// The height at which a color specimen shows every row it has; a host
    /// with more room would only add empty terminal under the last one.
    /// `None` for the other scenes.
    pub(crate) fn whole_color_sample_height(&self) -> Option<f32> {
        (self.scene == PreviewScene::Appearance).then(|| {
            let spec = self.normalized();
            (preview_header_height(&spec)
                + 15.0
                + appearance_terminal_floor(
                    spec.specimen_headroom()
                        + APPEARANCE_SPECIMEN_ROWS as f32 * spec.typography_row_height(),
                ))
            .ceil()
        })
    }

    /// A typography preview's height with `rows` whole specimen rows.
    fn typography_height(&self, rows: usize) -> f32 {
        let spec = self.normalized();
        preview_header_height(&spec)
            + 15.0
            + spec.specimen_headroom()
            + rows as f32 * spec.typography_row_height()
            + font_candidate_pill_height(crate::native_appearance::text_scale())
            + 10.0
    }

    /// The room above the specimen's first row that a display face's
    /// capitals rise into (the frame's head band,
    /// [`aterm_render::Renderer::display_ink_headroom`]), with a point of
    /// rounding slack; `0` for an ordinary font.
    fn specimen_headroom(&self) -> f32 {
        match self.prepared_font.specimen_ink_headroom(
            self.font_px,
            self.line_height,
            self.baseline_adjust,
        ) {
            Some(rise) if rise > 0 => rise as f32 + 1.0,
            _ => 0.0,
        }
    }

    fn typography_row_height(&self) -> f32 {
        let variations = self
            .typography
            .variations
            .iter()
            .map(|variation| (variation.tag, variation.value()))
            .collect::<Vec<_>>();
        // Native layout is in logical points. One point of rounding headroom
        // per row covers the renderer's device-pixel rounding at scales >=1;
        // the authored font size itself is never reduced or rounded here.
        self.prepared_font
            .specimen_cell_height(self.font_px, self.line_height, &variations)
            .map_or(self.font_px * self.line_height * 1.5, |height| {
                height as f32
            })
            + 1.0
    }

    fn terminal_specimen(&self, theme: Theme, rows: Option<usize>) -> TerminalSpecimenSpec {
        let (input, input_fingerprint) = build_terminal_specimen_input_rows(self, theme, rows);
        TerminalSpecimenSpec {
            input,
            input_fingerprint,
            motion: (self.scene == PreviewScene::CursorMotion)
                .then(|| Arc::new(CursorScene::new(self, theme))),
            prepared_font: self.prepared_font.clone(),
            theme,
            font_px: self.font_px,
            line_height: self.line_height,
            baseline_adjust: self.baseline_adjust,
            ligatures: self.ligatures,
            merged_ligatures: self.merged_ligatures,
            cursor_break_ligatures: self.typography.cursor_break_ligatures,
            synthetic_styles: self.synthetic_styles,
            underline_position: self.typography.underline_position,
            underline_thickness: self.typography.underline_thickness,
            underline_skip_descenders: self.typography.underline_skip_descenders,
            text_blending: self.typography.text_blending,
            font_thicken: self.typography.font_thicken,
            stem_gamma: self.typography.stem_gamma,
            variations: self.typography.variations.clone(),
            minimum_contrast: self.appearance.minimum_contrast,
            selection_foreground: self.appearance.selection_foreground,
            selection_inactive: self.appearance.selection_inactive,
        }
    }

    /// The side the WINDOW SAMPLE paints (`true` = Light) when this spec is
    /// painted against `host` — the theme [`Self::paint`] falls back to for a
    /// spec carrying no candidate palette. THE resolver ([`window_sample_light`])
    /// answering for the pixels, exposed so a fixture that needs a candidate on
    /// the OPPOSITE side can ask instead of restating the platform rule and
    /// silently rotting when the rule moves.
    #[cfg(test)]
    pub(crate) fn window_sample_is_light(&self, host: Theme) -> bool {
        let spec = self.normalized();
        let terminal_theme = spec
            .terminal_theme
            .map_or(host, PreviewTerminalTheme::into_theme);
        window_sample_light(&spec, terminal_theme)
    }

    pub(crate) fn paint(
        &self,
        prims: &mut Vec<DrawPrim>,
        rect: LogicalRect,
        theme: Theme,
        roles: Roles,
    ) {
        let spec = self.normalized();
        if rect.width <= 2.0 || rect.height <= 2.0 {
            return;
        }

        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            radius: 11.0,
            fill: rgba(roles.elevated, 255),
        });
        prims.push(DrawPrim::Stroke {
            x: rect.x + 0.5,
            y: rect.y + 0.5,
            w: (rect.width - 1.0).max(0.0),
            h: (rect.height - 1.0).max(0.0),
            radius: 10.5,
            width: 1.0,
            color: rgba(roles.separator, 190),
        });

        let inset = 10.0;
        let header_h = preview_header_height(&spec);
        let caption_size = TypeStep::Caption
            .px(13.75)
            .scaled(crate::native_appearance::text_scale());
        let caption_y =
            crate::tray_raster::row_baseline(rect.y + 4.0, header_h, caption_size.get());
        let scene_badge = spec.scene.badge_label();
        let scene_width = crate::tray_raster::ui_text_width_for(
            TextFace::UiBold,
            scene_badge,
            caption_size.get(),
        );
        let available = (rect.width - inset * 2.0).max(0.0);
        let measure = |s: &str| {
            crate::tray_raster::ui_text_width_for(TextFace::UiBold, s, caption_size.get())
        };
        let with_state = |candidate: String| spec.with_state_badge(candidate);
        // The whole badge wherever the header holds it; a long one shortens
        // its subject first, so the value being tried stays readable.
        let mut state = with_state(spec.candidate_badge());
        let mut state_width = measure(&state);
        if scene_width + 8.0 + state_width > available {
            state = with_state(spec.bounded_candidate_badge(30));
            state_width = measure(&state);
        }
        if scene_width + 8.0 + state_width > available {
            // A still sample's compact state IS its candidate, so the
            // candidate is said once, not twice.
            state = compact_header_state(&spec);
            state_width = measure(&state);
        }
        if state_width > available {
            let compact = spec.compact_state_badge();
            state = if compact == spec.candidate_badge() {
                spec.bounded_candidate_badge(24)
            } else {
                bounded_badge(&compact, 24)
            };
            state_width = measure(&state);
        }
        if state_width > available
            && let Some((_, value)) = spec.candidate_parts()
            && spec.compact_state_badge() == spec.candidate_badge()
        {
            // The subject goes before any of the value does.
            state = value;
            state_width = measure(&state);
        }
        if state_width > available {
            // Character caps are only a first-pass readability heuristic:
            // proportional glyphs and Dynamic Type make their pixel width
            // unknowable. The final preview-header projection therefore fits
            // the actual UI-bold measure and remains UTF-8 safe.
            state = bounded_badge_width(&state, available, caption_size.get());
            state_width =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, caption_size.get());
        }
        let show_scene = scene_width + 8.0 + state_width <= available;
        if show_scene {
            // The scene badge is the preview's kicker — it wears the theme
            // accent so every card header carries one confident spot of the
            // theme's own color (the state badge on the right keeps its
            // quieter semantic coloring).
            prims.push(text_prim(
                rect.x + inset,
                caption_y,
                scene_badge.to_string(),
                caption_size,
                TextWeight::Regular,
                TextFace::UiBold,
                // The accent as text, held to the text floor.
                rgba(roles.accent_ink, 255),
            ));
        }
        prims.push(text_prim(
            (rect.right() - inset - state_width).max(rect.x + inset),
            caption_y,
            state,
            caption_size,
            TextWeight::Regular,
            TextFace::UiBold,
            rgba(
                if matches!(spec.animation(), PreviewAnimation::Continuous) {
                    roles.success
                } else {
                    roles.text_secondary
                },
                255,
            ),
        ));

        let terminal = LogicalRect::new(
            rect.x + inset,
            rect.y + header_h + 5.0,
            (rect.width - inset * 2.0).max(1.0),
            (rect.height - header_h - inset - 5.0).max(1.0),
        );
        let terminal_theme = spec
            .terminal_theme
            .map_or(theme, PreviewTerminalTheme::into_theme);
        let background = crate::settings::u32_rgb(terminal_theme.bg);

        // A short host shows fewer COMPLETE rows at the authored size, never
        // shrunken glyphs, and never a clipped next row. Typography's status
        // pill owns its own band, so its glyphs never sit beneath the status;
        // the pill exists only while a font loads or fails, and where its
        // band would leave no whole row it yields to the glyphs, which are
        // the proof, unless it is warning that the font failed.
        let row = spec.typography_row_height();
        // A display face's capitals rise into a band above the first row.
        let head = spec.specimen_headroom();
        let pill_band = font_candidate_pill_height(crate::native_appearance::text_scale()) + 10.0;
        let mut shows_pill = false;
        let whole_rows = match spec.scene {
            PreviewScene::Appearance if terminal.height < appearance_terminal_floor(head + row) => {
                0
            }
            PreviewScene::Appearance => (((appearance_specimen_clip(terminal).height - head) / row)
                .floor() as usize)
                .min(APPEARANCE_SPECIMEN_ROWS),
            PreviewScene::Typography => {
                let with_pill =
                    ((terminal.height - head - pill_band).max(0.0) / row).floor() as usize;
                let bare = ((terminal.height - head - 4.0).max(0.0) / row).floor() as usize;
                let pill = spec.font_candidate_pill();
                let warning = pill.as_ref().is_some_and(|(_, warning)| *warning);
                shows_pill =
                    pill.is_some() && (with_pill > 0 || (warning && terminal.height >= pill_band));
                if shows_pill { with_pill } else { bare }.min(TYPOGRAPHY_SPECIMEN_ROWS)
            }
            PreviewScene::CursorMotion => (((terminal.height - head).max(0.0) / row).floor()
                as usize)
                .min(CURSOR_SPECIMEN_ROWS),
        };
        // A body that holds no whole row (and no status band) is a sliver:
        // the card keeps its header naming what it would show, and paints
        // nothing under it rather than a strip of cut glyphs.
        if whole_rows == 0 && !shows_pill {
            return;
        }
        prims.push(DrawPrim::Panel {
            x: terminal.x,
            y: terminal.y,
            w: terminal.width,
            h: terminal.height,
            radius: 8.0,
            fill: rgba(background, 255),
        });
        prims.push(DrawPrim::ClipPush {
            x: terminal.x,
            y: terminal.y,
            w: terminal.width,
            h: terminal.height,
        });
        if whole_rows > 0 {
            let terminal_specimen = spec.terminal_specimen(terminal_theme, Some(whole_rows));
            if spec.scene == PreviewScene::Appearance {
                let specimen_clip = appearance_specimen_clip(terminal);
                prims.push(DrawPrim::ClipPush {
                    x: specimen_clip.x,
                    y: specimen_clip.y,
                    w: specimen_clip.width,
                    h: specimen_clip.height,
                });
            }
            prims.push(DrawPrim::TerminalSpecimen {
                x: terminal.x,
                y: terminal.y,
                spec: Box::new(terminal_specimen),
            });
            if spec.scene == PreviewScene::Appearance {
                prims.push(DrawPrim::ClipPop);
            }
        }
        if spec.scene == PreviewScene::Appearance {
            paint_window_theme_sample(prims, terminal, &spec, terminal_theme);
        }
        if shows_pill {
            paint_font_candidate_pill(prims, terminal, &spec, roles);
        }
        prims.push(DrawPrim::ClipPop);
    }

    /// What the typing-momentum glow draws on this preview's terminal: a
    /// halo of light on a dark one; on a light one, where light cannot
    /// show, only the block caret's warmer fill, and nothing for a bar
    /// ([`aterm_effects::cursor_momentum`]). A preview with no terminal
    /// theme of its own follows the host's, and says the halo.
    fn typing_glow(&self) -> TypingGlow {
        match self.terminal_theme {
            Some(theme) if !theme_bg_is_dark(theme.bg) => match self.cursor.style {
                PreviewCursorStyle::Block => TypingGlow::Tint,
                PreviewCursorStyle::Bar => TypingGlow::Nothing,
            },
            _ => TypingGlow::Halo,
        }
    }

    fn state_badge(&self) -> String {
        if self.cursor.trail_pack_unavailable() {
            "Trail unavailable".to_string()
        } else if self.reduction_suppresses_motion() {
            self.pause.badge()
        } else {
            match self.animation() {
                PreviewAnimation::None => match self.scene {
                    PreviewScene::Appearance => "Color sample".to_string(),
                    PreviewScene::Typography => "Text sample".to_string(),
                    PreviewScene::CursorMotion => "Static preview".to_string(),
                },
                PreviewAnimation::BlinkEdge { .. } => "Live cursor blink".to_string(),
                PreviewAnimation::Continuous if !self.cursor.has_resolved_trail() => {
                    match self.typing_glow() {
                        TypingGlow::Halo => "Live typing glow",
                        TypingGlow::Tint => "Live typing tint",
                        TypingGlow::Nothing => "Live typing",
                    }
                    .to_string()
                }
                PreviewAnimation::Continuous => "Live trail".to_string(),
            }
        }
    }

    /// `candidate` followed by the state badge, unless the candidate already
    /// says it: a reduced-motion candidate says what the state badge would,
    /// and a theme's `Theme:`/`Sample:` already names the still colour
    /// sample (a pause or an unavailable trail still reads after it).
    fn with_state_badge(&self, candidate: String) -> String {
        let state = self.state_badge();
        let redundant = if self.focused_key == crate::prefs::EDIT_MOTION {
            self.reduction_suppresses_motion()
        } else {
            self.focused_key == crate::prefs::EDIT_THEME
                && self.scene == PreviewScene::Appearance
                && self.animation() == PreviewAnimation::None
                && !self.reduction_suppresses_motion()
                && !self.cursor.trail_pack_unavailable()
        };
        if redundant {
            candidate
        } else {
            format!("{candidate} · {state}")
        }
    }

    /// The focused setting as a short subject and a readable value: `Style`
    /// and `Rainbow kitty`, `Line height` and `1.6×`, `Ligatures` and
    /// `On`. `None` when the focus has no name to show.
    fn candidate_parts(&self) -> Option<(String, String)> {
        let key = self.focused_key.as_str();
        let subject = match key {
            crate::prefs::EDIT_THEME if self.theme_sample => "Sample",
            crate::prefs::EDIT_THEME => "Theme",
            crate::prefs::EDIT_WINDOW_THEME => "Window appearance",
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE => "Style",
            crate::prefs::EDIT_CURSOR_STYLE => "Shape",
            crate::prefs::EDIT_FONT_FAMILY => "Font",
            crate::prefs::EDIT_FONT_FAMILY_BOLD => "Bold font",
            crate::prefs::EDIT_FONT_FAMILY_ITALIC => "Italic font",
            crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC => "Bold italic font",
            crate::prefs::EDIT_FONT_PX => "Size",
            crate::prefs::EDIT_LINE_HEIGHT => "Line height",
            crate::prefs::EDIT_FONT_WEIGHT => "Weight",
            crate::prefs::EDIT_EMOJI_FONT => "Emoji font",
            crate::prefs::EDIT_SYMBOL_FONT => "Symbol font",
            crate::prefs::EDIT_FALLBACK_FONTS => "Fallback fonts",
            crate::prefs::EDIT_MOTION => "Motion",
            _ if !self.focused_label.is_empty() => self.focused_label.as_str(),
            _ => return None,
        };
        let raw = self.focused_value.as_str();
        let value = if let Some(family) = self.focused_font_slot() {
            // A name no resolver found is not what the sample draws: say
            // what it draws instead (the pill names the missing one).
            match self.unresolved_focused_slot() {
                Some("regular") => "Current font".to_string(),
                Some("bold" | "italic" | "bold italic") => "Regular face".to_string(),
                Some(_) => self.automatic_words(),
                None => family.map_or_else(|| self.automatic_words(), family_words),
            }
        } else if crate::prefs::edit_kind(key) == crate::prefs::EditKind::Bool {
            if raw.eq_ignore_ascii_case("true") {
                "On".to_string()
            } else {
                "Off".to_string()
            }
        } else if key == crate::prefs::EDIT_THEME
            && let Some(identity) = &self.terminal_theme_identity
            && (crate::prefs::theme_mode(Some(raw)) == crate::prefs::ThemeMode::FollowSystem
                || raw.strip_suffix(" (default)").unwrap_or(raw).trim()
                    == crate::prefs::UNSET_THEME_LABEL)
        {
            // A theme that follows the system (a pair, or unset) previews one
            // side: the theme drawn, never the raw `dark:…,light:…` value.
            identity.label.clone()
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE && !self.cursor.trail_enabled {
            // The cursor style reads Off while cursor effects are off.
            "Off".to_string()
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE
            && raw.trim() == self.cursor.effective_trail_style_raw()
        {
            // The style's name, as its row on Cursor Styles says it.
            self.cursor.trail_style_label()
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE {
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_MOTION {
            // The words the Motion control shows, not the raw value.
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_WINDOW_THEME {
            // The words the picker shows ("Match macOS"), not the raw value.
            let raw = raw.strip_suffix(" (default)").unwrap_or(raw);
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_LINE_HEIGHT {
            format!("{raw}×")
        } else if key == crate::prefs::EDIT_FONT_PX {
            // Unset, the value is the row's placeholder, "12 (automatic)":
            // the unit belongs to the number, the note after it.
            match raw.strip_suffix("(automatic)") {
                Some(px) => format!("{} px (automatic)", px.trim()),
                None => format!("{raw} px"),
            }
        } else if crate::prefs::edit_kind(key) == crate::prefs::EditKind::Color
            && let Some(color) = aterm_types::scheme::parse_hex(raw)
        {
            // A color reads as its hex, never sentence-cased ("Ff0000").
            format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
        } else if let Some(reading) = raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .and_then(|value| crate::prefs::numeric_reading(key, value, raw.trim()))
        {
            // A number reads with its unit, as its slider says it.
            reading
        } else {
            friendly_preview_value(raw)
        };
        Some((subject.to_string(), value))
    }

    fn candidate_badge(&self) -> String {
        self.candidate_parts().map_or_else(
            || friendly_preview_value(&self.focused_value),
            |(subject, value)| format!("{subject}: {value}"),
        )
    }

    /// [`Self::candidate_badge`] in at most `max` graphemes, shortening the
    /// subject first so the value being tried stays readable.
    fn bounded_candidate_badge(&self, max: usize) -> String {
        match self.candidate_parts() {
            Some((subject, value)) => {
                let full = format!("{subject}: {value}");
                if full.graphemes().count() <= max {
                    return full;
                }
                // A display-face mix says what it is in the row; a short
                // badge keeps the faces' names instead.
                let value = display_face_suffix_stripped(&value).unwrap_or(&value);
                let value_len = value.graphemes().count();
                if subject.graphemes().count() + 2 + value_len <= max {
                    return format!("{subject}: {value}");
                }
                // ": " and the "…" a shortened subject ends in come out of
                // the subject's share, never the value's; a subject with no
                // readable share left goes, and only a value too long to fit
                // at all is cut.
                let room = max.saturating_sub(value_len + 3);
                if room >= 4 {
                    format!("{}: {value}", bounded_badge(&subject, room))
                } else if value_len <= max {
                    value.to_string()
                } else {
                    bounded_badge(value, max)
                }
            }
            None => bounded_badge(&friendly_preview_value(&self.focused_value), max),
        }
    }

    /// Compact, user-facing glance label for narrow preview headers.
    fn compact_state_badge(&self) -> String {
        if self.cursor.trail_pack_unavailable() {
            "Trail unavailable".to_string()
        } else if self.reduction_suppresses_motion() {
            self.pause.badge()
        } else {
            match self.animation() {
                PreviewAnimation::None => self.candidate_badge(),
                PreviewAnimation::BlinkEdge { .. } => "Live blink".to_string(),
                PreviewAnimation::Continuous if !self.cursor.has_resolved_trail() => {
                    "Typing glow · Live".to_string()
                }
                PreviewAnimation::Continuous => {
                    format!("{} · Live", self.cursor.trail_style_label())
                }
            }
        }
    }
}

const TYPOGRAPHY_SPECIMEN_ROWS: usize = 5;

fn preview_header_height(spec: &SettingsPreviewSpec) -> f32 {
    if spec.scene == PreviewScene::Typography {
        27.0_f32.max(
            TypeStep::Caption
                .px(13.75)
                .scaled(crate::native_appearance::text_scale())
                .get()
                + 8.0,
        )
    } else {
        27.0
    }
}

fn font_candidate_pill_height(text_scale: f32) -> f32 {
    (TypeStep::Caption.px(11.5).scaled(text_scale).get() + 7.0).max(19.0)
}

fn paint_font_candidate_pill(
    prims: &mut Vec<DrawPrim>,
    terminal: LogicalRect,
    spec: &SettingsPreviewSpec,
    roles: Roles,
) {
    paint_font_candidate_pill_at_scale(
        prims,
        terminal,
        spec,
        roles,
        crate::native_appearance::text_scale(),
    );
}

fn paint_font_candidate_pill_at_scale(
    prims: &mut Vec<DrawPrim>,
    terminal: LogicalRect,
    spec: &SettingsPreviewSpec,
    roles: Roles,
    text_scale: f32,
) {
    let Some((label, warning)) = spec.font_candidate_pill() else {
        return;
    };
    let text_size = TypeStep::Caption.px(11.5).scaled(text_scale);
    let pill_h = font_candidate_pill_height(text_scale);
    if terminal.height < pill_h + 8.0 {
        // A pill squeezed under its own text height cuts its label in half;
        // the header still names the font.
        return;
    }
    // The terminal has a 5pt outer inset and the pill has 8pt text padding on
    // both sides: the label fits that exact pixel budget, whole wherever the
    // specimen is wide enough, and a display-face note goes before any of
    // the faces' names does.
    let budget = (terminal.width - 26.0).max(0.0);
    let label = if crate::tray_raster::ui_text_width_for(TextFace::UiBold, &label, text_size.get())
        <= budget
    {
        label
    } else {
        let short = display_face_suffix_stripped(&label).unwrap_or(&label);
        bounded_badge_width(short, budget, text_size.get())
    };
    let text_width =
        crate::tray_raster::ui_text_width_for(TextFace::UiBold, &label, text_size.get());
    let pill_w = (text_width + 16.0)
        .min((terminal.width - 10.0).max(1.0))
        .max(1.0);
    let x = terminal.x + 5.0;
    let y = (terminal.bottom() - pill_h - 5.0).max(terminal.y + 3.0);
    prims.push(DrawPrim::Panel {
        x,
        y,
        w: pill_w,
        h: pill_h,
        radius: 6.0,
        fill: rgba(roles.elevated, 248),
    });
    prims.push(DrawPrim::Stroke {
        x: x + 0.5,
        y: y + 0.5,
        w: (pill_w - 1.0).max(0.0),
        h: (pill_h - 1.0).max(0.0),
        radius: 5.5,
        width: 1.0,
        color: rgba(if warning { roles.danger } else { roles.accent }, 230),
    });
    prims.push(text_prim(
        x + 8.0,
        crate::tray_raster::row_baseline(y, pill_h, text_size.get()),
        label,
        text_size,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(roles.text_primary, 255),
    ));
}

/// Paint the window-control signature for the platform this binary targets and
/// return the leading inset available to title text, so the window sample never
/// claims macOS traffic lights on another platform.
fn paint_platform_window_controls(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    title_h: f32,
    light: bool,
) -> f32 {
    #[cfg(target_os = "macos")]
    {
        let _ = light;
        for (index, color) in [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]]
            .into_iter()
            .enumerate()
        {
            prims.push(DrawPrim::Dot {
                cx: x + 9.0 + index as f32 * 10.0,
                cy: y + title_h * 0.5,
                r: 3.0,
                color: rgba(color, 255),
            });
        }
        43.0
    }
    #[cfg(not(target_os = "macos"))]
    {
        prims.push(DrawPrim::Stroke {
            x: x + 7.0,
            y: y + (title_h - 7.0) * 0.5,
            w: 7.0,
            h: 7.0,
            radius: 1.0,
            width: 1.0,
            color: rgba(window_chrome_text_color(light), 210),
        });
        22.0
    }
}

/// Text/control ink conditioned against the preview titlebar itself. Native UI
/// roles are conditioned against the terminal-derived Settings surface, which
/// may have the opposite luminance from a forced Light/Dark window chrome.
const fn window_chrome_text_color(light: bool) -> [u8; 3] {
    if light {
        [0x64, 0x68, 0x72]
    } else {
        [0xB9, 0xBD, 0xC9]
    }
}

/// The one dark/light classifier for the preview — the trail sample and the
/// window sample both read it — delegating to [`aterm_render::theme_is_dark`],
/// which is itself the chrome's `ink::bg_is_light` inverted, so the preview,
/// the real chrome and the real effects can never disagree about a theme's
/// side.
fn theme_bg_is_dark(bg: u32) -> bool {
    aterm_render::theme_is_dark(bg)
}

/// The light/dark side the WINDOW SAMPLE paints for the spec's `window_theme`.
/// Explicit Light/Dark is itself. Auto is the PLATFORM truth: macOS/Windows
/// follow the live system appearance (`system_dark`), while Linux follows the
/// candidate TERMINAL THEME's darkness — the resolution the shipping chrome
/// actually uses there (`chrome_theme_for_apprt`/`chrome_palette_theme`; no
/// live OS-appearance feed exists under winit's Wayland backend). The Themes
/// stage ([`SettingsPreviewSpec::window_follows_theme`]) takes the candidate's
/// side on every platform: its sample is the theme, not the OS.
fn window_sample_light(spec: &SettingsPreviewSpec, terminal_theme: Theme) -> bool {
    match spec.appearance.window_theme.as_str() {
        "light" => true,
        "dark" => false,
        _ if spec.window_follows_theme || cfg!(target_os = "linux") => {
            !theme_bg_is_dark(terminal_theme.bg)
        }
        _ => !spec.system_dark,
    }
}

/// The color specimen's rows. Every card keeps the first ones first: ANSI
/// and weights, ligatures and the cursor, underline; a taller card adds the
/// palette at work, as a listing and status words and then a short shell
/// session (git status, a diff, a test run), so a tall card is a terminal
/// screen in the theme rather than an empty one.
const APPEARANCE_SPECIMEN_ROWS: usize = 16;
const APPEARANCE_SWATCH_BOTTOM_INSET: f32 = 10.0;
const APPEARANCE_SWATCH_HEIGHT: f32 = 6.0;
const APPEARANCE_SPECIMEN_SWATCH_GAP: f32 = 4.0;
/// The window sample's title bar, in the terminal's top right corner. The
/// palette swatches never ride up into it.
const APPEARANCE_WINDOW_SAMPLE_HEIGHT: f32 = 19.0;
const APPEARANCE_SWATCH_TOP_CLEARANCE: f32 = APPEARANCE_WINDOW_SAMPLE_HEIGHT + 1.0;

fn appearance_swatch_y(terminal: LogicalRect) -> f32 {
    terminal.bottom() - APPEARANCE_SWATCH_BOTTOM_INSET
}

fn appearance_specimen_clip(terminal: LogicalRect) -> LogicalRect {
    LogicalRect::new(
        terminal.x,
        terminal.y,
        terminal.width,
        (appearance_swatch_y(terminal) - APPEARANCE_SPECIMEN_SWATCH_GAP - terminal.y).max(0.0),
    )
}

/// The least terminal body a color specimen demonstrates in: one whole row
/// of `row` height above the palette swatches, and the swatches clear of
/// the window sample.
fn appearance_terminal_floor(row: f32) -> f32 {
    (row + APPEARANCE_SPECIMEN_SWATCH_GAP).max(APPEARANCE_SWATCH_TOP_CLEARANCE)
        + APPEARANCE_SWATCH_BOTTOM_INSET
}

fn paint_window_theme_sample(
    prims: &mut Vec<DrawPrim>,
    terminal: LogicalRect,
    spec: &SettingsPreviewSpec,
    terminal_theme: Theme,
) {
    let light = window_sample_light(spec, terminal_theme);
    let width = terminal.width.min(112.0);
    let x = terminal.right() - width;
    let fill = if light {
        [0xF1, 0xF2, 0xF5]
    } else {
        [0x20, 0x22, 0x2A]
    };
    // Only a body with room for the whole title bar draws it: a cut one
    // reads as a grey band, not a window.
    if terminal.height >= APPEARANCE_WINDOW_SAMPLE_HEIGHT + APPEARANCE_SWATCH_BOTTOM_INSET {
        prims.push(DrawPrim::Panel {
            x,
            y: terminal.y,
            w: width,
            h: APPEARANCE_WINDOW_SAMPLE_HEIGHT,
            radius: 0.0,
            fill: rgba(fill, 255),
        });
        let title_inset = paint_platform_window_controls(
            prims,
            x,
            terminal.y,
            APPEARANCE_WINDOW_SAMPLE_HEIGHT,
            light,
        );
        prims.push(DrawPrim::Line {
            x1: x + title_inset,
            y1: terminal.y + APPEARANCE_WINDOW_SAMPLE_HEIGHT / 2.0,
            x2: terminal.right() - 8.0,
            y2: terminal.y + APPEARANCE_WINDOW_SAMPLE_HEIGHT / 2.0,
            width: 2.0,
            color: rgba(window_chrome_text_color(light), 210),
        });
    }

    // The semantic terminal specimen remains the primary renderer proof, but a
    // freshly opened Settings tab may need one asynchronous font-preparation
    // turn before glyphs land. Keep the small Top card useful on that first
    // frame with a renderer-native monospace label and literal ANSI swatches.
    // These use the candidate palette and therefore cannot collapse into the
    // blank background-only sample the old 132pt card produced.
    let candidate = spec
        .terminal_theme
        .unwrap_or_else(|| PreviewTerminalTheme::over_engine_palette(terminal_theme));
    if terminal.height >= 42.0 && !spec.prepared_font.is_ready() {
        let sample_size = TypeStep::Caption.px(11.5);
        prims.push(text_prim(
            terminal.x + 8.0,
            terminal.y + 37.0,
            "$ aterm".to_string(),
            sample_size,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(crate::settings::u32_rgb(candidate.fg), 255),
        ));
        prims.push(text_prim(
            terminal.x + 67.0,
            terminal.y + 37.0,
            "ready".to_string(),
            sample_size,
            TextWeight::Bold,
            TextFace::Mono,
            rgba(crate::settings::u32_rgb(candidate.ansi[10]), 255),
        ));
    }
    // All sixteen ANSI colours in index order: the normal row then the
    // bright row, so a theme's dim colours are judged as well as its accents.
    let swatch_count = candidate.ansi.len() as f32;
    let swatch_room = (terminal.width - 16.0).max(0.0);
    // A card too narrow for gaps drops them rather than overflowing.
    let swatch_gap = if swatch_room >= swatch_count * 4.0 {
        2.0
    } else {
        0.0
    };
    let swatch_width = (swatch_room - swatch_gap * (swatch_count - 1.0)) / swatch_count;
    let swatch_y = appearance_swatch_y(terminal);
    if swatch_y < terminal.y + APPEARANCE_SWATCH_TOP_CLEARANCE {
        // Never a palette pushed up through the window sample.
        return;
    }
    for (index, color) in candidate.ansi.iter().copied().enumerate() {
        prims.push(DrawPrim::Panel {
            x: terminal.x + 8.0 + index as f32 * (swatch_width + swatch_gap),
            y: swatch_y,
            w: swatch_width,
            h: APPEARANCE_SWATCH_HEIGHT,
            radius: 2.0,
            fill: rgba(crate::settings::u32_rgb(color), 255),
        });
    }
}

/// `color` packed as `0x00RRGGBB`; the inverse of [`packed_rgb`].
pub(crate) const fn pack_rgb(color: aterm_types::Rgb) -> u32 {
    ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
}

fn packed_rgb(value: u32) -> aterm_types::Rgb {
    aterm_types::Rgb::new(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    )
}

#[cfg(test)]
fn build_terminal_specimen_input(
    spec: &SettingsPreviewSpec,
    theme: Theme,
) -> (Arc<aterm_render::RenderInput>, u64) {
    build_terminal_specimen_input_rows(spec, theme, None)
}

fn build_terminal_specimen_input_rows(
    spec: &SettingsPreviewSpec,
    theme: Theme,
    visible_rows: Option<usize>,
) -> (Arc<aterm_render::RenderInput>, u64) {
    use std::hash::{Hash, Hasher};

    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::{CursorStyle, Terminal};

    const FULL_SOURCE: &str = concat!(
        "\u{1b}[31mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
        "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
        "\u{1b}[4mgypq_j underline\u{1b}[0m\r\n",
        "CJK 你好  symbols ∑✓♥\r\n",
        "emoji 😀 🚀 👩‍💻"
    );
    // The first three rows are what every color card keeps; a taller one
    // shows the palette at work, as a listing and as status words, then a
    // short shell session. Every line fits the specimen's 32 columns, so
    // none wraps into the row under it.
    const APPEARANCE_SOURCE: &str = concat!(
        "\u{1b}[31mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
        "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
        "\u{1b}[4mgypq_j underline\u{1b}[0m\r\n",
        "\u{1b}[1;34mdocs/\u{1b}[0m  \u{1b}[1;32mbuild.sh\u{1b}[0m  \u{1b}[35mlogo.png\u{1b}[0m  ",
        "\u{1b}[36mlink\u{1b}[0m\r\n",
        "\u{1b}[31merror\u{1b}[0m \u{1b}[33mwarning\u{1b}[0m \u{1b}[32mpassed\u{1b}[0m \u{1b}[34mnote\u{1b}[0m\r\n",
        "\u{1b}[32m$\u{1b}[0m git status --short\r\n",
        "\u{1b}[32mM\u{1b}[0m  src/theme.rs\r\n",
        " \u{1b}[31mM\u{1b}[0m README.md\r\n",
        "\u{1b}[31m??\u{1b}[0m notes.txt\r\n",
        "\u{1b}[32m$\u{1b}[0m git diff\r\n",
        "\u{1b}[36m@@ -8,2 +8,2 @@\u{1b}[0m fn main()\r\n",
        "\u{1b}[31m-    let x = 1;\u{1b}[0m\r\n",
        "\u{1b}[32m+    let x = 2;\u{1b}[0m\r\n",
        "\u{1b}[32m$\u{1b}[0m make test\r\n",
        "\u{1b}[32mok\u{1b}[0m 42 passed  \u{1b}[33m1 ignored\u{1b}[0m\r\n",
        "\u{1b}[32m$\u{1b}[0m "
    );
    let cursor_scene = spec.scene == PreviewScene::CursorMotion;
    let (rows, source) = match spec.scene {
        PreviewScene::Appearance => (APPEARANCE_SPECIMEN_ROWS, APPEARANCE_SOURCE.to_string()),
        PreviewScene::Typography => (TYPOGRAPHY_SPECIMEN_ROWS, FULL_SOURCE.to_string()),
        // The cursor scene keeps its NEWEST lines, so the prompt it types on
        // is always shown, with the room below it a terminal has once there
        // are three rows to spend.
        PreviewScene::CursorMotion => {
            let rows = visible_rows
                .unwrap_or(CURSOR_SPECIMEN_ROWS)
                .clamp(1, CURSOR_SPECIMEN_ROWS);
            let lines = if rows >= 3 { rows - 1 } else { rows };
            (
                rows,
                CURSOR_SPECIMEN_LINES[CURSOR_SPECIMEN_LINES.len() - lines..].join("\r\n"),
            )
        }
    };
    let rows = visible_rows.unwrap_or(rows).clamp(1, rows);
    let source = source
        .match_indices("\r\n")
        .nth(rows - 1)
        .map_or(source.as_str(), |(end, _)| &source[..end]);

    // The memo key comes FIRST because it is a total function of this call's
    // inputs — nothing below reads anything that is not hashed here.
    //
    // INVARIANT: every value the build below consults must appear in this hash.
    // The cursor scene's moving parts are not in this snapshot: they are played
    // onto it at raster time ([`CursorScene::decorate`]), so a live preview
    // keeps hitting this memo while its phase moves.
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    rows.hash(&mut hash);
    theme.fg.hash(&mut hash);
    theme.bg.hash(&mut hash);
    theme.cursor.hash(&mut hash);
    theme.selection.hash(&mut hash);
    (spec.scene as u8).hash(&mut hash);
    spec.terminal_theme
        .map(|candidate| candidate.ansi)
        .hash(&mut hash);
    spec.appearance.selection_foreground.hash(&mut hash);
    spec.appearance.bold_is_bright.hash(&mut hash);
    spec.appearance.faint_opacity.to_bits().hash(&mut hash);
    (spec.cursor.style as u8).hash(&mut hash);
    spec.cursor.blink.hash(&mut hash);
    (!cursor_scene && spec.cursor_visible()).hash(&mut hash);
    specimen_cursor_col(spec).hash(&mut hash);
    let fingerprint = hash.finish() | 1;

    // Building the snapshot means constructing a whole `aterm_core::Terminal`
    // (grid page store included) and re-parsing the const specimen through the
    // real VT parser, which is pure but far from free at 30 fps. The result is
    // an immutable `Arc` snapshot (`TerminalSpecimenSpec::input`; raster only
    // ever reads it), so returning the previous frame's `Arc` for an identical
    // key is indistinguishable from rebuilding it.
    //
    // The slot is thread-local, so a raster worker can never observe another
    // thread's entry, and it stores the full (key, value) pair: a bare value
    // keyed elsewhere is exactly how such caches go stale.
    thread_local! {
        static LAST_SPECIMEN: std::cell::RefCell<Option<(u64, Arc<aterm_render::RenderInput>)>> =
            const { std::cell::RefCell::new(None) };
    }
    if let Some(cached) = LAST_SPECIMEN.with_borrow(|slot| match slot {
        Some((key, input)) if *key == fingerprint => Some(Arc::clone(input)),
        _ => None,
    }) {
        return (cached, fingerprint);
    }

    let mut terminal_config = aterm_core::config::TerminalConfig {
        default_foreground: packed_rgb(theme.fg),
        default_background: packed_rgb(theme.bg),
        cursor_color: Some(packed_rgb(theme.cursor)),
        selection_background: Some(packed_rgb(theme.selection)),
        selection_foreground: spec.appearance.selection_foreground.map(packed_rgb),
        bold_is_bright: spec.appearance.bold_is_bright,
        faint_opacity: spec.appearance.faint_opacity,
        // The blink lives IN the style (`CursorStyle` names shape + blink, and
        // the renderer arms its blink clock off the `Blinking*` variants), so
        // the preview folds `spec.cursor.blink` into the shape. Setting the
        // sibling `cursor_blink` alone left the preview steady while the
        // settings pane said the cursor blinks.
        cursor_style: match spec.cursor.style {
            PreviewCursorStyle::Bar => CursorStyle::SteadyBar,
            PreviewCursorStyle::Block => CursorStyle::SteadyBlock,
        }
        .with_blink(spec.cursor.blink),
        cursor_blink: spec.cursor.blink,
        ..aterm_core::config::TerminalConfig::default()
    };
    if let Some(candidate) = spec.terminal_theme {
        let mut palette = aterm_types::ColorPalette::new();
        for (index, color) in candidate.ansi.into_iter().enumerate() {
            palette.set(index as u8, packed_rgb(color));
        }
        terminal_config.custom_palette = Some(palette);
    }
    let mut terminal = Terminal::new(rows as u16, SPECIMEN_COLS as u16);
    let _ = terminal.apply_config(&terminal_config);
    terminal.process(source.as_bytes());
    let mut input = terminal.cell_frame(rows, SPECIMEN_COLS);
    input.cursor_style = terminal_config.cursor_style;
    input.cursor_color = theme.cursor;
    input.default_bg = theme.bg;
    if cursor_scene {
        // The caret, the typed text and every effect are the scene's, played
        // at raster time on the renderer's own cell grid.
        input.cursor_row = prompt_row(&input);
        input.cursor_col = 0;
        input.cursor_visible = false;
    } else {
        input.cursor_row = 1.min(rows - 1);
        input.cursor_col = specimen_cursor_col(spec);
        // The caret belongs to the second row; a one-row specimen has none.
        input.cursor_visible = spec.cursor_visible() && rows > 1;
        let mut selection = aterm_core::selection::TextSelection::new();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 5, SelectionSide::Right);
        selection.complete_selection();
        input.selection = selection;
    }

    let input = Arc::new(input);
    LAST_SPECIMEN.with_borrow_mut(|slot| *slot = Some((fingerprint, Arc::clone(&input))));
    (input, fingerprint)
}

/// Columns of every terminal specimen.
const SPECIMEN_COLS: usize = 32;

/// Where a still specimen's cursor sits on its second row: just after the
/// word `cursor`, so the `!= => ->` run stays readable. Only while Break
/// ligatures at cursor is being tried does it sit inside the `!=`, to show
/// the split a terminal makes while editing an operator.
fn specimen_cursor_col(spec: &SettingsPreviewSpec) -> usize {
    if spec.focused_key == crate::prefs::EDIT_CURSOR_BREAK_LIGATURES {
        1
    } else {
        "!= => -> ITAL B+I cursor".len()
    }
}

/// The cursor scene's rows: its transcript and the blank row below the prompt.
const CURSOR_SPECIMEN_ROWS: usize = CURSOR_SPECIMEN_LINES.len() + 1;

/// The row the cursor scene types on: its last row with text.
fn prompt_row(input: &RenderInput) -> usize {
    input
        .cells
        .iter()
        .rposition(|row| row.iter().any(|cell| cell.ch != ' '))
        .unwrap_or(input.rows.saturating_sub(1))
}

/// The cursor scene's shell, oldest line first. The last line is the prompt
/// the scene types its command on, one key at a time; a short card keeps the
/// newest lines so the prompt is always shown.
const CURSOR_SPECIMEN_LINES: [&str; 7] = [
    "\u{1b}[32m~/demo\u{1b}[0m $ git pull",
    "Already up to date.",
    "\u{1b}[32m~/demo\u{1b}[0m $ ls",
    "\u{1b}[1;34msrc\u{1b}[0m  notes.md  \u{1b}[1;32mbuild.sh\u{1b}[0m",
    "\u{1b}[32m~/demo\u{1b}[0m $ cat notes.md",
    "ship it \u{2713}",
    "\u{1b}[32m~/demo\u{1b}[0m $ echo hello world",
];
/// One scripted keystroke every this many milliseconds (about 75 words a
/// minute), so the command is typed across the whole phase cycle.
const KEY_MS: u64 = 3 * MOTION_STEP_MS;
/// Keys already typed when the phase cycle starts, so its first frame already
/// has a trail behind the caret.
const WARMUP_KEYS: u64 = 1;
/// The scripted key the Flying kitty arrives on, about halfway through the
/// command: in the terminal the kitty is earned by a run of steady typing
/// (`aterm_effects::kitty_cursor`), so the preview shows the ribbon alone
/// first and the kitty flying in partway through, never a kitty that is
/// simply always there.
const FLYING_ARRIVES_AT_KEY: usize = 9;
/// How long the arriving Flying kitty takes to fade in, as the terminal's
/// does (`aterm_effects::kitty_cursor`'s fade-in).
const FLYING_FADE_IN_MS: u64 = 350;

/// The prompt row of the cursor scene: where the command starts and ends.
#[derive(Clone, Copy, Debug)]
struct TypedLine {
    /// First column after the `$ ` prompt.
    start: usize,
    /// One past the command's last cell.
    end: usize,
}

impl TypedLine {
    fn of(row: &[aterm_core::terminal::RenderCell]) -> Self {
        let end = row
            .iter()
            .rposition(|cell| cell.ch != ' ')
            .map_or(0, |last| last + 1);
        let start = row
            .windows(2)
            .position(|pair| pair[0].ch == '$' && pair[1].ch == ' ')
            .map_or(0, |dollar| dollar + 2)
            .min(end);
        Self { start, end }
    }

    /// Keys typed by scripted time `t_ms`.
    fn typed_at(self, t_ms: u64) -> usize {
        usize::try_from(t_ms / KEY_MS)
            .unwrap_or(usize::MAX)
            .min(self.end - self.start)
    }

    fn caret(self, typed: usize) -> usize {
        self.start + typed
    }
}

/// What rides with the caret in the cursor scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneCompanion {
    None,
    Pet(aterm_effects::kitty_pet::PetSpecies),
    FlyingKitty,
}

/// The cursor scene's moving parts — typed text, caret, trail, bloom and
/// companion — resolved at paint and played at raster time against the
/// renderer's own cell metrics, so they land on the text grid the specimen
/// draws. The shipping renderer composites every effect stream with its own
/// blend mode, under-ink at the under-ink seam, exactly as a live terminal.
#[derive(Clone, Debug)]
pub(crate) struct CursorScene {
    cursor: CursorPreviewSpec,
    post_fx: CursorPostFxSpec,
    theme: Theme,
    phase_ms: u64,
    /// The scripted typing plays; otherwise the line stands finished and still.
    typing: bool,
    /// The blink phase (and the cursor style) show the caret.
    caret_shown: bool,
    companion: SceneCompanion,
    /// The Flying kitty is the selected style's, so it is earned partway
    /// through the typing ([`FLYING_ARRIVES_AT_KEY`]); the sprite control's
    /// diagnostic kitty shows at once.
    flying_earned: bool,
    /// Total over every field above, for the raster frame memo.
    pub(crate) fingerprint: u64,
}

impl CursorScene {
    fn new(spec: &SettingsPreviewSpec, theme: Theme) -> Self {
        use std::hash::{Hash, Hasher};

        let sprite_diagnostic = spec.focused_key == crate::prefs::EDIT_CURSOR_NYAN_SPRITE;
        let companion = if !spec.cursor.trail_enabled {
            SceneCompanion::None
        } else if sprite_diagnostic {
            // The sprite control previews the asset it edits even when the
            // selected companion is a resident pet. This focused specimen is
            // diagnostic, never a claim that the flying sprite is active.
            if spec.reduced_motion {
                SceneCompanion::None
            } else {
                SceneCompanion::FlyingKitty
            }
        } else {
            match spec.cursor.trail_companion() {
                // A resident pet stays as a settled still under Reduced Motion,
                // exactly like runtime.
                PreviewTrailCompanion::Pet(species) => SceneCompanion::Pet(species),
                // Flying-kitty episodes are motion and stay suppressed under
                // Reduced Motion, exactly like runtime.
                PreviewTrailCompanion::FlyingKitty if !spec.reduced_motion => {
                    SceneCompanion::FlyingKitty
                }
                PreviewTrailCompanion::FlyingKitty | PreviewTrailCompanion::None => {
                    SceneCompanion::None
                }
            }
        };
        let mut hash = aterm_hash::FxHasher::default();
        spec.paint_fingerprint().hash(&mut hash);
        theme.fg.hash(&mut hash);
        theme.bg.hash(&mut hash);
        theme.cursor.hash(&mut hash);
        theme.selection.hash(&mut hash);
        Self {
            cursor: spec.cursor.clone(),
            post_fx: spec.post_fx.clone(),
            theme,
            phase_ms: spec.phase_ms,
            typing: spec.animation() == PreviewAnimation::Continuous,
            caret_shown: spec.cursor_visible(),
            companion,
            flying_earned: companion == SceneCompanion::FlyingKitty && !sprite_diagnostic,
            fingerprint: hash.finish() | 1,
        }
    }

    /// The scripted typing plays, so every frame differs.
    pub(crate) fn is_moving(&self) -> bool {
        self.typing
    }

    /// Play the scene onto `base` for a renderer whose cells are `cell` device
    /// pixels: type the command up to this phase, run the shared cursor-family
    /// step along it, and place the companion beside the caret.
    pub(crate) fn decorate(&self, base: &RenderInput, cell: (usize, usize)) -> RenderInput {
        let mut input = base.clone();
        let (cell_w, cell_h) = cell;
        let (rows, cols) = (input.rows, input.cols);
        if rows == 0 || cols == 0 || cell_w == 0 || cell_h == 0 || input.cells.len() < rows {
            return input;
        }
        let row = prompt_row(&input);
        let line = TypedLine::of(&input.cells[row]);
        let end_ms = WARMUP_KEYS * KEY_MS + self.phase_ms;
        let typed = if self.typing {
            line.typed_at(end_ms)
        } else {
            line.end - line.start
        };
        let blank = aterm_core::terminal::RenderCell {
            fg: crate::settings::u32_rgb(self.theme.fg),
            bg: crate::settings::u32_rgb(self.theme.bg),
            ..aterm_core::terminal::RenderCell::default()
        };
        for cell in input.cells[row]
            .iter_mut()
            .take(line.end)
            .skip(line.caret(typed))
        {
            *cell = blank;
        }
        let caret = (row as u16, line.caret(typed).min(cols - 1) as u16);
        input.cursor_row = usize::from(caret.0);
        input.cursor_col = usize::from(caret.1);
        input.cursor_visible = self.caret_shown;
        let geom = Geom {
            cw: cell_w,
            ch: cell_h,
            rows,
            cols,
            origin_x: 0,
            origin_y: 0,
            win_w: u16::try_from(cols.saturating_mul(cell_w)).unwrap_or(u16::MAX),
            win_h: u16::try_from(rows.saturating_mul(cell_h)).unwrap_or(u16::MAX),
            head: 0,
        };
        if self.typing {
            self.play(&mut input, base, (row, line), geom, end_ms);
            self.post_glow(&mut input, geom, caret);
        }
        let last_key_ms = typed as u64 * KEY_MS;
        let settled_ms = if self.typing {
            end_ms.saturating_sub(last_key_ms)
        } else {
            u64::MAX
        };
        self.place_companion(&mut input, cell, caret, typed, settled_ms);
        input
    }

    /// Run the cursor family from the first key to `end_ms`, one 52 ms frame
    /// at a time, and hand its streams and caret to `input`.
    fn play(
        &self,
        input: &mut RenderInput,
        base: &RenderInput,
        (row, line): (usize, TypedLine),
        geom: Geom,
        end_ms: u64,
    ) {
        use aterm_effects::cursor_fx::{compose_caret_style_override, head_dx_for};

        let mut glow = self
            .cursor
            .glow_config(self.theme, head_dx_for(input.cursor_style));
        let presentation = self.cursor.resolved_trail_presentation();
        let mut comet = TrailConfig {
            enabled: self.cursor.trail_enabled && presentation.comet,
            duration: Duration::from_millis(self.cursor.duration_ms),
            max_len: self.cursor.length,
            color: self
                .cursor
                .color
                .unwrap_or(crate::cursor_glow::COMET_DEFAULT_COLOR),
            // Ignited per frame from the typing cadence, as in a window.
            intensity: 0.0,
            warmth: 0.0,
        };
        // As in a window (`App::cursor_frame`): automatic colours follow the
        // cursor colour the scene draws, so the preview shows the colour the
        // terminal will.
        aterm_effects::cursor_fx::follow_live_cursor(
            &mut glow,
            &mut comet,
            self.theme.cursor,
            self.cursor.color.is_some(),
            self.cursor.accent.is_some(),
        );
        let probe_of = |cells: &[aterm_core::terminal::RenderCell], upto: usize| {
            cells
                .iter()
                .enumerate()
                .map(|(col, cell)| match cell {
                    _ if col >= upto => ' ',
                    cell if cell.wide => '\0',
                    cell => cell.ch,
                })
                .collect::<Vec<_>>()
        };
        let above = (row > 0).then(|| probe_of(&base.cells[row - 1], usize::MAX));
        let mut fx = CursorFx::default();
        let origin = Instant::now();
        let mut typed_before = 0;
        let mut t = 0;
        let frame = loop {
            let now = origin + Duration::from_millis(t);
            let typed = line.typed_at(t);
            let keys = typed - typed_before;
            fx.begin(true, true);
            for _ in 0..keys {
                fx.cadence.on_keystroke(now);
                fx.momentum
                    .on_key(now, aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S);
                fx.glow.note_typed_cells(now, 1);
            }
            if keys > 0 {
                fx.trail
                    .note_typed_batch(now, u32::try_from(keys).unwrap_or(u32::MAX));
            }
            let caret = line.caret(typed).min(geom.cols - 1) as u16;
            fx.glow.observe_row_with_trust(
                row as u16,
                caret,
                &probe_of(&base.cells[row], line.caret(typed)),
                now,
                crate::cursor_glow::ProbeTrust::Full,
            );
            fx.glow.observe_neighbor_rows(above.as_deref(), None);
            fx.glow.observe_caret_drawn(true);
            let frame = fx.tick(
                &CursorFxInput {
                    now,
                    cur: Some((row as u16, caret)),
                    live_viewport: true,
                    cursor_style: input.cursor_style,
                    blink_phase: true,
                    live_cursor: self.theme.cursor,
                    cursor_color_pinned: self.cursor.cursor_color_pinned,
                    default_bg: self.theme.bg,
                    geom,
                    focused: true,
                    amplitude: 1.0,
                    shed_envelope: 1.0,
                    body_allowed: true,
                    master: self.cursor.trail_enabled,
                    momentum_glow: self.cursor.momentum_glow,
                    user_tinted: self.cursor.color.is_some(),
                },
                &glow,
                comet,
            );
            typed_before = typed;
            if t >= end_ms {
                break frame;
            }
            t = (t + MOTION_STEP_MS).min(end_ms);
        };
        input.cursor_glow_add = std::mem::take(&mut fx.glow_scratch);
        input.glow_under = fx.glow.under_quads().to_vec();
        input.glow_halo = fx.glow.halos().to_vec();
        input.fire_patch = fx.glow.patches().to_vec();
        input.char_fg = fx.glow.charred().to_vec();
        input.fire_halo = fx.glow.halo_cells().to_vec();
        input.cursor_trail = std::mem::take(&mut fx.trail_scratch);
        input.cursor_trail_color = frame.trail_color;
        input.cursor_fill_override = frame.block_fill.map(|owned| owned.fill);
        input.cursor_effect_style_override = compose_caret_style_override(
            frame.bolt_cursor,
            frame.twinkle_cursor,
            frame.momentum_steady,
        );
        // A caret an effect holds steady does not blink, whatever the phase.
        if input
            .cursor_effect_style_override
            .is_some_and(|style| !style.blinks())
        {
            input.cursor_visible = true;
        }
    }

    /// The portable stand-ins for the GPU-only bloom and fire shimmer: additive
    /// light, so like the real thing they only show on a dark ground.
    fn post_glow(&self, input: &mut RenderInput, geom: Geom, caret: (u16, u16)) {
        if !input.cursor_visible {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let cx = f32::from(caret.1) * cw + cw * 0.5;
        let cy = f32::from(caret.0) * ch + ch * 0.5;
        let luma = aterm_render::hdr::packed_luma(self.theme.bg);
        let sdr_budget = aterm_render::hdr::sdr_glow_budget(luma, self.post_fx.sdr_boost);
        let hdr_tone = if self.post_fx.hdr_glow { 0.18 } else { 0.0 };
        let strength = if self.post_fx.bloom {
            self.post_fx.bloom_strength / 3.0
        } else {
            0.0
        };
        let alpha = ((sdr_budget + hdr_tone) * strength * 255.0)
            .round()
            .clamp(0.0, 150.0);
        if alpha > 0.0 {
            let radius = ch * (0.22 + self.post_fx.bloom_radius * 0.12) * 1.15;
            let peak = aterm_render::premul_rgb(self.theme.cursor, (alpha * 2.0).min(255.0) as u8);
            push_additive_halo(&mut input.glow_halo, geom, (cx, cy), radius, peak);
        }
        if self.post_fx.fire_shimmer && self.cursor.trail_style == PreviewTrailStyle::Fire {
            let wobble =
                (((self.phase_ms / MOTION_STEP_MS) % 5) as f32 - 2.0) * (cw / 8.0).max(1.0);
            let (row, y) = if caret.0 > 0 {
                (caret.0 - 1, (f32::from(caret.0) * ch - 3.0).max(0.0))
            } else {
                (0, 1.0)
            };
            input.cursor_glow_add.push(GlowQuad::flat(
                row,
                (cx - cw * 0.8 + wobble).max(0.0) as u16,
                y as u16,
                (cw * 1.6).max(1.0) as u16,
                2,
                aterm_render::premul_rgb(0x00FF_8A24, 80),
                0,
            ));
        }
    }

    /// Draw the companion beside the caret on its row: the pet stands at its
    /// station just past the caret, walking while keys land and sitting once
    /// they stop; the flying kitty leads the caret along its ribbon.
    fn place_companion(
        &self,
        input: &mut RenderInput,
        (cell_w, cell_h): (usize, usize),
        caret: (u16, u16),
        typed: usize,
        settled_ms: u64,
    ) {
        use aterm_effects::word_decorations::{EffectGeom, KittySpriteSource, WordDecorations};

        if self.companion == SceneCompanion::None || self.flying_alpha(typed, settled_ms) == 0 {
            return;
        }
        let asset = &self.post_fx.kitty_asset;
        if self.companion == SceneCompanion::FlyingKitty
            && matches!(asset, crate::app_config::KittySpriteAsset::Invalid { .. })
        {
            return;
        }
        let (Ok(cell_w), Ok(cell_h), Ok(rows), Ok(cols)) = (
            u16::try_from(cell_w),
            u16::try_from(cell_h),
            u16::try_from(input.rows),
            u16::try_from(input.cols),
        ) else {
            return;
        };
        let geom = EffectGeom {
            cell_w,
            cell_h,
            rows,
            cols,
        };
        let colors = aterm_effects::cat_baker::CatColorKey::from_rgb(
            self.theme.bg,
            self.theme.fg,
            self.theme.fg,
        );
        thread_local! {
            // The companion bakes are costly and keyed by size inside the engine,
            // so one engine per raster thread serves every frame; a new custom
            // sprite replaces it.
            static COMPANIONS: std::cell::RefCell<Option<(u64, WordDecorations)>> =
                const { std::cell::RefCell::new(None) };
        }
        COMPANIONS.with_borrow_mut(|slot| {
            let key = asset.fingerprint();
            if slot.as_ref().is_none_or(|(held, _)| *held != key) {
                let mut decorations = WordDecorations::default();
                if let crate::app_config::KittySpriteAsset::Ready { w, h, rgba, fp, .. } = asset {
                    decorations.set_kitty_sprite_source(KittySpriteSource::Custom {
                        source_fp: *fp,
                        w: *w,
                        h: *h,
                        rgba: Arc::clone(rgba),
                    });
                }
                *slot = Some((key, decorations));
            }
            let Some((_, decorations)) = slot.as_mut() else {
                return;
            };
            let mut sprites = Vec::new();
            // Each emission is its own frame with a fresh bake budget. A first
            // sight of a look can need a second budget, and a still preview
            // has no next frame to wait for.
            for _ in 0..3 {
                decorations.begin_host_frame();
                self.emit_companion(
                    decorations,
                    (geom, colors),
                    caret,
                    typed,
                    settled_ms,
                    1.0,
                    &mut sprites,
                );
                if !sprites.is_empty() {
                    break;
                }
            }
            let floor = i32::from(rows) * i32::from(cell_h);
            // Every companion is taller than one row (the flying head and the
            // pet both stand 1.7 rows): a one-row card (a short window) draws
            // it smaller and whole, the pet still on its feet, rather than cut
            // at the card's edge. It is baked already, so the redraw is a
            // scale.
            let tallest = sprites.iter().map(|sprite| i32::from(sprite.h)).max();
            if let Some(tallest) = tallest.filter(|tallest| *tallest > floor) {
                let mut fitted = Vec::new();
                decorations.begin_host_frame();
                self.emit_companion(
                    decorations,
                    (geom, colors),
                    caret,
                    typed,
                    settled_ms,
                    floor as f32 / tallest as f32,
                    &mut fitted,
                );
                if !fitted.is_empty() {
                    sprites = fitted;
                }
            }
            if let Some(atlas) = decorations.free_atlas()
                && !sprites.is_empty()
            {
                // A short card's prompt is its last row: lift a head that would
                // hang below the frame back inside it rather than clip it.
                for sprite in &mut sprites {
                    sprite.y = sprite.y.min(floor - i32::from(sprite.h)).max(0);
                }
                input.free_sprites = sprites;
                input.free_atlas = Some(atlas);
            }
        });
    }

    /// The Flying kitty's opacity with `typed` keys down and `settled_ms`
    /// since the last: nothing before it is earned, then a fade-in (full on a
    /// still preview, whose line stands typed). Full for every other
    /// companion.
    fn flying_alpha(&self, typed: usize, settled_ms: u64) -> u8 {
        if !self.flying_earned {
            return 255;
        }
        let Some(past) = typed.checked_sub(FLYING_ARRIVES_AT_KEY) else {
            return 0;
        };
        let since = (past as u64)
            .saturating_mul(KEY_MS)
            .saturating_add(settled_ms);
        u8::try_from(since.saturating_mul(255) / FLYING_FADE_IN_MS).unwrap_or(255)
    }

    /// One companion frame. `fit` scales the companion (1.0: its own size),
    /// for a card too short to hold it: the flying head about its centre,
    /// the pet about its feet, its left edge kept at its station.
    #[allow(clippy::too_many_arguments)] // One companion frame's placement facts.
    fn emit_companion(
        &self,
        decorations: &mut aterm_effects::word_decorations::WordDecorations,
        (geom, colors): (
            aterm_effects::word_decorations::EffectGeom,
            aterm_effects::cat_baker::CatColorKey,
        ),
        caret: (u16, u16),
        typed: usize,
        settled_ms: u64,
        fit: f32,
        sprites: &mut Vec<aterm_core::render::FreeSprite>,
    ) {
        use aterm_effects::kitty_pet::{
            PET_DEPARTURES_MAX, PET_MOTES_MAX, PetAction, PetBrain, PetFrame,
        };
        use aterm_effects::pet_glyphs_gen::PetGlyphId;
        use aterm_effects::word_decorations::{KittyCursorFrame, PetCursorFrame};

        let (cell_w, cell_h, cols) = (geom.cell_w, geom.cell_h, geom.cols);
        match self.companion {
            SceneCompanion::Pet(species) => {
                // Walking while keys land, standing a beat, then sitting.
                let (action, pose) = if settled_ms < KEY_MS * 2 {
                    let walk = [
                        PetGlyphId::PetWalk0,
                        PetGlyphId::PetWalk1,
                        PetGlyphId::PetWalk2,
                        PetGlyphId::PetWalk3,
                    ];
                    (PetAction::Walk, walk[typed % walk.len()])
                } else if settled_ms < 500 {
                    (PetAction::Stand, PetGlyphId::PetStand)
                } else {
                    (PetAction::Sit, PetGlyphId::PetSit)
                };
                let width = aterm_effects::kitty_pet::art_cols(cell_w, cell_h);
                // The engine squashes a body about its feet, centred on its
                // natural box: shift that box so the fitted body still starts
                // at the station, one cell past the caret.
                let station = PetBrain::station(caret.1, cols, width * fit);
                let col = station - width * (1.0 - fit) / 2.0;
                let _ = decorations.pet_cursor(
                    PetCursorFrame {
                        geom,
                        colors,
                        // The coat and eyes the window's companion wears, as
                        // the terminal's pet does (the dog too: it reads as a
                        // dog by its shape).
                        coat: self.post_fx.companion_look.coat,
                        iris: self.post_fx.companion_look.iris,
                        pet: PetFrame {
                            alpha: 255,
                            lane_alpha: 255,
                            action,
                            pose: species.skin(pose),
                            col,
                            row: f32::from(caret.0),
                            lift: 0.0,
                            facing_left: false,
                            scale_x: fit,
                            scale_y: fit,
                            purr: 0.0,
                            contented: false,
                            under_ink: false,
                            motes: [None; PET_MOTES_MAX],
                            departures: [None; PET_DEPARTURES_MAX],
                        },
                    },
                    sprites,
                );
            }
            SceneCompanion::FlyingKitty => {
                let _ = decorations.kitty_cursor(
                    KittyCursorFrame {
                        geom,
                        cursor: caret,
                        look: self.post_fx.companion_look,
                        colors,
                        bob: 0.0,
                        alpha: self.flying_alpha(typed, settled_ms),
                        pose: aterm_effects::kitty_cursor::CatPose {
                            scale_x: fit,
                            scale_y: fit,
                            ..aterm_effects::kitty_cursor::CatPose::STILL
                        },
                        // A preview still: no celebration, no notes.
                        sing: 0.0,
                        notes: [None; aterm_effects::kitty_sing::MAX_NOTES],
                    },
                    sprites,
                );
            }
            SceneCompanion::None => {}
        }
    }
}

/// One additive radial light centred at `(cx, cy)`, split into the per-row
/// bands every halo stream requires.
fn push_additive_halo(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    (cx, cy): (f32, f32),
    radius: f32,
    color: u32,
) {
    let r = radius.round().max(1.0) as i32;
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let (Ok(cxu), Ok(cyu), Ok(ru)) = (u16::try_from(cxi), u16::try_from(cyi), u16::try_from(r))
    else {
        return;
    };
    let x0 = (cxi - r).max(0);
    let x1 = (cxi + r).min(i32::from(geom.win_w));
    let ch = geom.ch as i32;
    let mut y = (cyi - r).max(0);
    let y1 = (cyi + r).min(i32::from(geom.win_h));
    while y < y1 && x1 > x0 {
        let band_row = y / ch;
        let band_end = ((band_row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: band_row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            color,
            cx: cxu,
            cy: cyu,
            rx: ru,
            ry: ru,
            mode: HaloMode::Add,
        });
        y = band_end;
    }
}

/// Fold a string's trimmed, ASCII-lowercased form into `hash` without
/// materializing it.
///
/// `normalized()` lowercases `window_theme` and `motion_raw`, so the paint
/// identity must hash the lowered bytes; doing it in place keeps the per-frame
/// key allocation-free. The trailing sentinel is load-bearing: `Hash for str`
/// appends a terminator after the bytes, and a hand-rolled byte loop must
/// supply its own or two adjacent string fields become concatenation-collidable
/// ("auto" + "full" hashing the same as "autof" + "ull").
fn hash_trimmed_lowercase<H: std::hash::Hasher>(value: &str, hash: &mut H) {
    for byte in value.trim().bytes() {
        H::write_u8(hash, byte.to_ascii_lowercase());
    }
    H::write_u8(hash, 0xff);
}

/// Canonical construction helper for Settings workbenches. The returned node is
/// a single semantic object with stable bounds regardless of visual settings.
pub(crate) fn preview_node(
    key: impl Into<std::sync::Arc<str>>,
    spec: SettingsPreviewSpec,
    height: f32,
) -> UiNode {
    UiNode::new(key, UiContent::SettingsPreview(Box::new(spec))).layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height.max(96.0)))
            .padding(Insets::default())
            .clipped(),
    )
}

/// Return the longest complete-grapheme prefix that fits a terminal row.
/// Wide CJK/emoji clusters consume two columns and ZWJ/combining sequences are
/// never split at the preview clip edge.
#[cfg(test)]
fn terminal_prefix(value: &str, columns: usize) -> &str {
    if columns == 0 {
        return "";
    }
    let mut used = 0_usize;
    let mut end = 0_usize;
    for (offset, grapheme) in value.grapheme_indices() {
        let width = grapheme_display_width(grapheme);
        if used.saturating_add(width) > columns {
            break;
        }
        used = used.saturating_add(width);
        end = offset.saturating_add(grapheme.len());
    }
    &value[..end]
}

#[cfg(test)]
fn grapheme_at_terminal_column(value: &str, column: usize) -> Option<&str> {
    let mut left = 0_usize;
    for grapheme in value.graphemes() {
        let width = grapheme_display_width(grapheme);
        let right = left.saturating_add(width);
        if width > 0 && column >= left && column < right {
            return Some(grapheme);
        }
        left = right;
    }
    None
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// The right-hand header badge once the full `candidate · state` line does
/// not fit: the candidate (shortened) and the compact glance. A static
/// preview's glance IS the candidate, so it is said once, never
/// `Theme: Nord · Theme: Nord`.
fn compact_header_state(spec: &SettingsPreviewSpec) -> String {
    let glance = spec.compact_state_badge();
    if glance == spec.candidate_badge() {
        spec.bounded_candidate_badge(30)
    } else {
        format!("{} · {glance}", spec.bounded_candidate_badge(20))
    }
}

fn bounded_badge(value: &str, max_graphemes: usize) -> String {
    let mut end = 0;
    for (count, (offset, grapheme)) in value.grapheme_indices().enumerate() {
        if count == max_graphemes {
            return format!("{}…", value[..end].trim_end());
        }
        end = offset + grapheme.len();
    }
    value.to_string()
}

fn bounded_badge_width(value: &str, max_width: f32, px: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if crate::tray_raster::ui_text_width_for(TextFace::UiBold, value, px) <= max_width {
        return value.to_string();
    }
    let ellipsis = "…";
    let ellipsis_width = crate::tray_raster::ui_text_width_for(TextFace::UiBold, ellipsis, px);
    if ellipsis_width > max_width {
        return String::new();
    }
    let mut end = 0;
    for (offset, grapheme) in value.grapheme_indices() {
        let next = offset + grapheme.len();
        if crate::tray_raster::ui_text_width_for(TextFace::UiBold, &value[..next], px)
            + ellipsis_width
            > max_width
        {
            break;
        }
        end = next;
    }
    format!("{}…", value[..end].trim_end())
}

/// A font family as the reader knows it: a `display:` family by its faces'
/// names, any other by its own.
fn family_words(family: &str) -> String {
    let Some(ids) = family.strip_prefix(aterm_render::DISPLAY_FACE_SCHEME) else {
        return family.to_string();
    };
    let names = ids
        .split('+')
        .filter_map(|id| {
            aterm_render::DISPLAY_FACES
                .iter()
                .find(|face| face.id == aterm_render::display_face_canonical_id(id).unwrap_or(id))
                .map(|face| face.label)
        })
        .collect::<Vec<_>>();
    match names.as_slice() {
        [] => family.to_string(),
        [one] => format!("{one} (display face)"),
        many => format!("{} (display faces)", many.join(" + ")),
    }
}

/// `value` without the " (display face)" note [`family_words`] gives a
/// display-face family, for a badge too short for the note.
fn display_face_suffix_stripped(value: &str) -> Option<&str> {
    value
        .strip_suffix(" (display faces)")
        .or_else(|| value.strip_suffix(" (display face)"))
}

fn friendly_preview_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Default".to_string();
    }
    let value = value
        .strip_prefix("pack:")
        .map(|id| format!("Trail Pack {id}"))
        .unwrap_or_else(|| value.replace(['_', '-'], " "));
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return "Default".to_string();
    };
    first.to_uppercase().chain(chars).collect()
}

fn trim_float(value: f32) -> String {
    if value.fract().abs() < 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Settings ▸ Themes' gallery cards: their data, built once per catalog
/// generation, and their paint-only specimen tiles.
#[path = "settings_preview_theme_card.rs"]
pub(crate) mod theme_card;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::native_ui::{GroupSpec, SemanticRole, UiTree};

    const VIEWPORT: LogicalRect = LogicalRect::new(0.0, 0.0, 480.0, 210.0);
    const NORD: PreviewTerminalTheme = PreviewTerminalTheme::new(
        0xD8_DEE9,
        0x2E_3440,
        0x88_C0D0,
        0x43_4C5E,
        [
            0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0,
            0x596377, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
        ],
    );

    /// The cursor scene played onto its specimen as raster time plays it, at
    /// an 8×17 cell.
    fn played(spec: &SettingsPreviewSpec, theme: Theme) -> RenderInput {
        let spec = spec.normalized();
        let (base, _) = build_terminal_specimen_input(&spec, theme);
        CursorScene::new(&spec, theme).decorate(&base, (8, 17))
    }

    fn has_effect_light(input: &RenderInput) -> bool {
        !input.cursor_glow_add.is_empty()
            || !input.glow_under.is_empty()
            || !input.glow_halo.is_empty()
            || !input.fire_patch.is_empty()
            || !input.cursor_trail.is_empty()
    }

    fn compiled(spec: SettingsPreviewSpec) -> crate::native_ui::CompiledUi {
        compiled_with_font(spec, |_| {})
    }

    /// [`compiled`], with a last chance to perturb the host-prepared semantic
    /// font snapshot the fixture injects. Only [`pixels_recomputed`] uses the
    /// hook, and only to move `snapshot.ready_epoch` — the one field of that
    /// snapshot that reaches no primitive at all (see the helper).
    fn compiled_with_font(
        mut spec: SettingsPreviewSpec,
        perturb: impl FnOnce(&mut crate::tray_raster::PreparedSemanticFont),
    ) -> crate::native_ui::CompiledUi {
        let _ = crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences::default(),
        );
        thread_local! {
            // A libtest worker can execute another font-mutating test between
            // two tests in this module. Track the actual cascade generation,
            // not a one-way boolean, so worker reuse cannot leave this pixel
            // fixture rendering somebody else's font.
            static INSTALLED_EPOCH: Cell<Option<u64>> = const { Cell::new(None) };
        }
        INSTALLED_EPOCH.with(|installed| {
            let current = crate::tray_raster::chrome_font_epoch_for_test();
            if installed.get() != Some(current) {
                let mut renderer = aterm_render::Renderer::from_bytes(
                    aterm_render::embedded_font(),
                    14.0,
                    Theme::default(),
                )
                .expect("embedded semantic renderer");
                renderer.set_runtime_font_discovery(false);
                renderer.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
                // SETTLED install, not `set_chrome_fonts`: the production seam
                // parks an async prewarm worker whose landing between two
                // `pixels()` calls of one test would swap the semantic cascade
                // mid-assertion (the pass-isolated/fail-parallel flake). The
                // fixture runs that warmup inline and parks no worker.
                let epoch = crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
                installed.set(Some(epoch));
            }
        });
        let mut prepared =
            crate::tray_raster::prepared_semantic_font_for_direct_view_test(&spec.font_candidate);
        perturb(&mut prepared);
        spec = spec.with_prepared_font(prepared);
        let preview = preview_node("preview", spec, 190.0);
        UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(10.0)))
            .children(vec![preview]),
        )
        .compile(VIEWPORT)
        .expect("preview compiles")
    }

    fn pixels(spec: SettingsPreviewSpec) -> Vec<u8> {
        raster_of(&compiled(spec).tray(Theme::default(), 13.0).prims)
    }

    fn raster_of(prims: &[DrawPrim]) -> Vec<u8> {
        crate::tray_raster::rasterize_tray(
            prims,
            VIEWPORT.width as u32,
            VIEWPORT.height as u32,
            1.0,
            [0, 0, 0, 0],
        )
        .0
    }

    /// [`pixels`], with BOTH specimen memos on the paint path forced to miss,
    /// so the terminal specimen — by far the most expensive prim in the tray —
    /// is genuinely re-derived instead of replayed.
    ///
    /// Two caches sit between a spec and its pixels: the single-slot engine
    /// snapshot memo inside [`build_terminal_specimen_input`], and
    /// `tray_raster`'s retained specimen frame (an `Rc<Frame>` keyed by the
    /// whole specimen spec beside the fork-source pointer). Both keys are total
    /// functions of one paint's inputs, so a SECOND paint of one spec hits both
    /// and hands back byte-identical bytes by construction — which would make
    /// any `differences(..) == 0` assertion across two paints trivially true for
    /// everything inside the specimen. Use this for the second half of such a
    /// comparison; both memos then miss, and the assertion is once again a
    /// statement about the renderer rather than about the cache:
    ///
    /// * the snapshot memo holds exactly one `(key, value)` pair, so parking
    ///   another key in it evicts whatever the previous paint left there. The
    ///   parking spec below is a different `PreviewScene`, which selects both a
    ///   different row count and a different source string — three of the hashed
    ///   inputs — and the returned fingerprint is asserted against the one this
    ///   paint actually resolves to, so a memo hit cannot pass unnoticed.
    /// * the retained frame is keyed on `prepared_font.snapshot.ready_epoch`
    ///   among the rest. That epoch is an identity token: it reaches no
    ///   primitive and no renderer setting — only `paint_fingerprint` and the
    ///   two raster memo keys — so moving it cannot change a pixel, but it does
    ///   force `cached_specimen_frame` to miss and the fork + `render_input` to
    ///   run again against the same immutable renderer `Rc`. The nonce makes the
    ///   key one this libtest worker has never retained, so a reused worker
    ///   cannot serve one recomputed paint from another's entry.
    fn pixels_recomputed(spec: SettingsPreviewSpec) -> Vec<u8> {
        let parking_spec = if spec.scene == PreviewScene::Appearance {
            SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
        } else {
            SettingsPreviewSpec::appearance(14.0)
        };
        let (_, parked) = build_terminal_specimen_input(&parking_spec, Theme::default());

        thread_local! {
            static RECOMPUTE_NONCE: Cell<u64> = const { Cell::new(0) };
        }
        let prims = compiled_with_font(spec, |prepared| {
            let nonce = RECOMPUTE_NONCE.with(|slot| {
                let next = slot.get().wrapping_add(1);
                slot.set(next);
                next
            });
            prepared.snapshot.ready_epoch = prepared.snapshot.ready_epoch.wrapping_add(nonce);
        })
        .tray(Theme::default(), 13.0)
        .prims;

        let fingerprint = prims.iter().find_map(|primitive| match primitive {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input_fingerprint),
            _ => None,
        });
        assert!(
            fingerprint.is_some_and(|resolved| resolved != parked),
            "the scene must paint a terminal specimen, and this paint must rebuild \
             its own engine snapshot instead of inheriting the parked one"
        );
        raster_of(&prims)
    }

    fn differences(left: &[u8], right: &[u8]) -> usize {
        left.iter().zip(right).filter(|(a, b)| a != b).count()
    }

    fn compile_pack(source: &str) -> TrailParams {
        *aterm_effects::trail_pack::compile_trail_pack_toml(source)
            .expect("Trail Pack compiles")
            .params()
    }

    fn panel_fill(prims: &[DrawPrim], wanted_radius: f32) -> [u8; 4] {
        prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::Panel { radius, fill, .. }
                    if (*radius - wanted_radius).abs() < f32::EPSILON =>
                {
                    Some(*fill)
                }
                _ => None,
            })
            .expect("preview panel exists")
    }

    fn test_relative_luminance(color: [u8; 3]) -> f32 {
        let linear = |channel: u8| {
            let value = f32::from(channel) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
    }

    fn test_contrast_ratio(left: [u8; 3], right: [u8; 3]) -> f32 {
        let left = test_relative_luminance(left);
        let right = test_relative_luminance(right);
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }

    #[test]
    fn phone_preview_header_keeps_scene_and_live_state_disjoint() {
        let roles = Roles::from_theme(Theme::default());
        let rect = LogicalRect::new(16.0, 150.0, 254.5, 120.0);
        for spec in [
            SettingsPreviewSpec::appearance(24.0),
            SettingsPreviewSpec::typography(18.0),
            SettingsPreviewSpec::default().with_phase(780),
        ] {
            let mut prims = Vec::new();
            spec.paint(&mut prims, rect, Theme::default(), roles);
            let headers = prims
                .iter()
                .filter_map(|primitive| match primitive {
                    DrawPrim::Text { x, s, px, face, .. } if *face == TextFace::UiBold => {
                        Some((*x, s.as_str(), *px))
                    }
                    _ => None,
                })
                .take(2)
                .collect::<Vec<_>>();
            assert!(
                !headers.is_empty(),
                "phone preview retains a candidate glance label"
            );
            let (right_x, right, right_px) = *headers.last().unwrap();
            assert!(
                !right.contains('='),
                "header exposes a raw config assignment: {right}"
            );
            assert!(
                !right.contains('_'),
                "header exposes a raw config key: {right}"
            );
            if headers.len() == 2 {
                let (left_x, left, left_px) = headers[0];
                let left_right =
                    left_x + crate::tray_raster::ui_text_width_for(TextFace::UiBold, left, left_px);
                assert!(
                    left_right + 7.99 <= right_x,
                    "preview header overlaps: {left:?} ends at {left_right:.1}, {right:?} starts at {right_x:.1}"
                );
            }
            assert!(
                right_x + crate::tray_raster::ui_text_width_for(TextFace::UiBold, right, right_px)
                    <= rect.right() - 10.0 + 0.01,
                "preview state stays inside its right inset"
            );
        }
    }

    #[test]
    fn maximum_text_preview_badges_fit_by_ui_width_and_preserve_utf8() {
        // 286.5pt compact host, less page/card/header insets. Caption is
        // 13.75pt at the supported 2× maximum.
        let available = 214.5;
        let px = 27.5;
        for source in [
            "Theme: Catppuccin Mocha",
            "Theme: Aurora 👩‍💻 Observatory — a deliberately very long personal color theme",
            "Trail: Trail Pack hyper-dimensional-rainbow-kitties",
        ] {
            let fitted = bounded_badge_width(source, available, px);
            assert!(std::str::from_utf8(fitted.as_bytes()).is_ok());
            assert!(fitted.ends_with('…'), "{source:?} => {fitted:?}");
            assert!(
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, &fitted, px)
                    <= available + 0.01,
                "{source:?} => {fitted:?}"
            );
        }
        assert_eq!(
            bounded_badge_width("Theme: Nord", available, px),
            "Theme: Nord"
        );
    }

    #[test]
    fn preview_badge_limits_preserve_complete_grapheme_clusters() {
        for (source, expected) in [("A👩‍💻WWWW", "A👩‍💻…"), ("AéWWWW", "Aé…"), ("A🇺🇳WWWW", "A🇺🇳…")]
        {
            assert_eq!(bounded_badge(source, 2), expected);
            let px = 27.5;
            let exact_width =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, expected, px) + 0.01;
            assert_eq!(
                bounded_badge_width(source, exact_width, px),
                expected,
                "{source:?}"
            );
        }
    }

    /// A shortened badge gives up its subject, never the value being tried.
    #[test]
    fn a_shortened_badge_keeps_the_value_whole() {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
            .with_focus(crate::prefs::EDIT_LOAD_ADAPTIVE_MOTION, "true")
            .with_focus_label("Reduce motion under heavy load");
        for max in [20, 24, 30] {
            let badge = spec.bounded_candidate_badge(max);
            assert!(badge.ends_with(": On"), "{max}: {badge}");
            assert!(badge.graphemes().count() <= max, "{max}: {badge}");
        }
        assert_eq!(
            spec.bounded_candidate_badge(30),
            "Reduce motion under heavy…: On"
        );
        // One that fits is whole.
        assert_eq!(
            spec.bounded_candidate_badge(40),
            "Reduce motion under heavy load: On"
        );

        // A display-face mix keeps every face's name before its note.
        let mix = SettingsPreviewSpec::typography(12.0)
            .with_focus(
                crate::prefs::EDIT_FONT_FAMILY,
                "display:chunky+pixel+engraved",
            )
            .with_font_candidate(SemanticFontCandidate {
                regular: Some("display:chunky+pixel+engraved".to_string()),
                ..SemanticFontCandidate::default()
            });
        assert_eq!(
            mix.candidate_badge(),
            "Font: Chunky + Pixel + Engraved (display faces)"
        );
        assert_eq!(
            mix.bounded_candidate_badge(40),
            "Font: Chunky + Pixel + Engraved"
        );
        assert_eq!(mix.bounded_candidate_badge(30), "Chunky + Pixel + Engraved");
    }

    /// A header with room says the whole badge: the 30-character cap is for
    /// headers that need it, not a wide one with 400pt to spare.
    #[test]
    fn a_wide_preview_header_says_the_whole_badge() {
        let family = "display:chunky+pixel+engraved";
        let spec = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, family)
            .with_font_candidate(SemanticFontCandidate {
                regular: Some(family.to_string()),
                ..SemanticFontCandidate::default()
            });
        let header = |width: f32| {
            let mut prims = Vec::new();
            spec.paint(
                &mut prims,
                LogicalRect::new(0.0, 0.0, width, 132.0),
                Theme::default(),
                Roles::from_theme(Theme::default()),
            );
            prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { s, baseline, .. }
                        if *baseline < 27.0 && s != PreviewScene::Typography.badge_label() =>
                    {
                        Some(s.clone())
                    }
                    _ => None,
                })
                .expect("the header names the font")
        };
        assert_eq!(
            header(720.0),
            "Font: Chunky + Pixel + Engraved (display faces) · Text sample"
        );
        // A narrow one shortens, keeping the faces' names.
        let narrow = header(320.0);
        assert!(narrow.contains("Chunky + Pixel + Engraved"), "{narrow}");
    }

    /// An unset font size reads with its unit on the number: "12 px
    /// (automatic)", not "12 (automatic) px".
    #[test]
    fn an_automatic_font_size_reads_with_its_unit_on_the_number() {
        let unset = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_PX, "12 (automatic)");
        assert_eq!(unset.candidate_badge(), "Size: 12 px (automatic)");
        let set =
            SettingsPreviewSpec::typography(14.0).with_focus(crate::prefs::EDIT_FONT_PX, "14");
        assert_eq!(set.candidate_badge(), "Size: 14 px");
    }

    #[test]
    fn narrow_large_type_font_candidate_pill_contains_its_unicode_text() {
        let family = "Aurora 👩‍💻 Observatory élan 🇺🇳 — deliberately long candidate family";
        let mut spec = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, family);
        spec.font_status = "unavailable; fallback active".to_string();
        let terminal = LogicalRect::new(12.0, 20.0, 112.0, 72.0);
        let mut prims = Vec::new();
        paint_font_candidate_pill_at_scale(
            &mut prims,
            terminal,
            &spec,
            Roles::from_theme(Theme::default()),
            2.0,
        );

        let panel = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .expect("font candidate pill panel");
        let text = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { x, s, px, face, .. } => Some((*x, s.as_str(), *px, *face)),
                _ => None,
            })
            .expect("font candidate pill text");
        assert!(text.1.ends_with('…'), "{:?}", text.1);
        assert!(std::str::from_utf8(text.1.as_bytes()).is_ok());
        assert!(panel.0 >= terminal.x);
        assert!(panel.1 >= terminal.y);
        assert!(panel.0 + panel.2 <= terminal.right() + 0.01);
        assert!(panel.1 + panel.3 <= terminal.bottom() + 0.01);
        let text_right = text.0 + crate::tray_raster::ui_text_width_for(text.3, text.1, text.2);
        assert!(
            text_right <= panel.0 + panel.2 - 8.0 + 0.01,
            "text {:?} ends at {text_right:.2} outside pill {:?}",
            text.1,
            panel
        );
    }

    #[test]
    fn a_narrow_static_header_names_its_candidate_once() {
        let spec =
            SettingsPreviewSpec::appearance(24.0).with_focus(crate::prefs::EDIT_THEME, "Nord");
        assert_eq!(spec.animation(), PreviewAnimation::None);
        let candidate = spec.bounded_candidate_badge(30);
        assert_eq!(compact_header_state(&spec), candidate);
        let moving = SettingsPreviewSpec::default().with_focus(crate::prefs::EDIT_THEME, "Nord");
        if moving.animation() != PreviewAnimation::None {
            let state = compact_header_state(&moving);
            assert!(state.starts_with(&candidate) && state.len() > candidate.len());
        }
    }

    #[test]
    fn reduced_motion_copy_only_appears_when_motion_was_actually_suppressed() {
        for static_spec in [
            SettingsPreviewSpec::appearance(24.0).with_reduced_motion(true),
            SettingsPreviewSpec::typography(24.0).with_reduced_motion(true),
        ] {
            assert_eq!(static_spec.animation(), PreviewAnimation::None);
            assert!(matches!(
                static_spec.state_badge().as_str(),
                "Color sample" | "Text sample"
            ));
            assert!(!static_spec.semantic_value().contains("reduced motion"));
        }

        let moving = SettingsPreviewSpec::default().with_reduced_motion(true);
        assert_eq!(moving.animation(), PreviewAnimation::None);
        assert_eq!(moving.state_badge(), "Reduced motion");
        // The person's own choice reads as the Motion picker names it.
        let chosen = moving.clone().with_pause(PreviewPause::MotionSetting);
        assert_eq!(
            chosen.state_badge(),
            crate::native_settings::reduce_motion_words()
        );
        assert!(
            chosen
                .semantic_value()
                .contains("paused because Motion is set to Reduce motion"),
            "{}",
            chosen.semantic_value()
        );
        let system = moving.clone().with_pause(PreviewPause::SystemReduceMotion);
        assert_eq!(system.state_badge(), "System Reduce Motion");
        // The stated cause is the real one.
        let background = moving.clone().with_pause(PreviewPause::Unfocused);
        assert_eq!(background.state_badge(), "Paused");
        assert!(background.semantic_value().contains("background"));
        assert!(!moving.semantic_value().contains("background"));
    }

    /// Every trail leaves visible marks in the theme it is previewed in, light
    /// or dark: the specimen renderer composites each effect stream with its
    /// own blend mode, so a light ground gets ink where a dark one gets light.
    /// The caret and companion are held equal; only the trail streams differ.
    #[test]
    fn every_trail_style_is_visible_on_light_and_dark_themes() {
        let _ = compiled(SettingsPreviewSpec::default());
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        let themes = [
            (
                "GitHub Light",
                PreviewTerminalTheme::from_scheme(
                    &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
                ),
            ),
            (
                "Default",
                PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()),
            ),
        ];
        let mut nonce = 0x5EED_0000_u64;
        let mut frame = |spec: &crate::widget::TerminalSpecimenSpec, input: RenderInput| {
            nonce += 1;
            let spec = crate::widget::TerminalSpecimenSpec {
                input: Arc::new(input),
                input_fingerprint: nonce,
                motion: None,
                ..spec.clone()
            };
            crate::tray_raster::rasterize_tray(
                &[DrawPrim::TerminalSpecimen {
                    x: 0.0,
                    y: 0.0,
                    spec: Box::new(spec),
                }],
                300,
                120,
                1.0,
                [0, 0, 0, 255],
            )
            .0
        };
        for (name, theme) in themes {
            for &style in crate::prefs::CURSOR_TRAIL_STYLES {
                if style == "off" {
                    continue;
                }
                let mut best = 0;
                for phase in [300, 900, 1_500, 2_100] {
                    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                        blink: false,
                        trail_style: PreviewTrailStyle::parse(style),
                        trail_style_raw: Arc::from(style),
                        ..CursorPreviewSpec::default()
                    })
                    .with_terminal_theme(theme)
                    .with_phase(phase)
                    .with_prepared_font(prepared.clone())
                    .normalized();
                    let specimen = spec.terminal_specimen(theme.into_theme(), Some(5));
                    let cell = crate::tray_raster::specimen_cell_size_for_test(&specimen, 1.0)
                        .expect("the fixture renderer forks");
                    let scene = specimen.motion.clone().expect("the cursor scene");
                    let lit = scene.decorate(&specimen.input, cell);
                    let mut bare = lit.clone();
                    bare.cursor_glow_add.clear();
                    bare.glow_under.clear();
                    bare.glow_halo.clear();
                    bare.fire_patch.clear();
                    bare.char_fg.clear();
                    bare.fire_halo.clear();
                    bare.cursor_trail.clear();
                    let shown = differences(&frame(&specimen, lit), &frame(&specimen, bare)) / 4;
                    best = best.max(shown);
                }
                assert!(
                    best > 40,
                    "{style} leaves only {best} visible pixels on {name}"
                );
            }
        }
    }

    /// Spoken summaries name what the preview shows: the cursor's shape and
    /// trail, the theme a colour sample shows, the text setting being tried —
    /// never a raw value standing in for a font or a theme.
    #[test]
    fn preview_summaries_name_the_setting_in_plain_words() {
        let cursor = |style, trail: &'static str| {
            SettingsPreviewSpec::cursor(CursorPreviewSpec {
                style,
                blink: false,
                trail_style: PreviewTrailStyle::parse(trail),
                trail_style_raw: Arc::from(trail),
                ..CursorPreviewSpec::default()
            })
            .with_phase(900)
            .semantic_value()
        };
        let bar = cursor(PreviewCursorStyle::Bar, "lumen");
        assert_ne!(bar, cursor(PreviewCursorStyle::Block, "lumen"), "{bar}");
        assert_ne!(bar, cursor(PreviewCursorStyle::Bar, "comet"), "{bar}");

        // The window appearance is not a colour theme.
        let window = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_terminal_theme_identity(PreviewThemeIdentity {
                label: "Nord".to_string(),
                light: false,
            })
            .with_appearance(AppearancePreviewSpec {
                window_theme: "light".to_string(),
                ..AppearancePreviewSpec::default()
            })
            .with_focus(crate::prefs::EDIT_WINDOW_THEME, "light")
            .semantic_value();
        assert!(
            window.contains("Nord, dark") && window.contains("Light window appearance"),
            "{window}"
        );

        // A Follow system pair names the side's theme it draws.
        let pair = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_terminal_theme_identity(PreviewThemeIdentity {
                label: "Nord".to_string(),
                light: false,
            })
            .with_focus(crate::prefs::EDIT_THEME, "dark:Nord,light:GitHub Light");
        assert_eq!(pair.candidate_badge(), "Theme: Nord");
        // A theme only the preview draws reads as a sample, not as the theme.
        let sample = pair.clone().with_theme_sample(true);
        assert_eq!(sample.candidate_badge(), "Sample: Nord");
        assert_ne!(sample.paint_fingerprint(), pair.paint_fingerprint());

        // A switch reads as on or off, never as the raw boolean.
        let ligatures = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_LIGATURES, "true")
            .with_focus_label("Ligatures");
        for text in [ligatures.candidate_badge(), ligatures.semantic_value()] {
            assert!(!text.to_lowercase().contains("true"), "{text}");
        }
        // A number is named with its setting.
        let line_height = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_LINE_HEIGHT, "1.6")
            .with_focus_label("Line height");
        for text in [line_height.candidate_badge(), line_height.semantic_value()] {
            assert!(text.to_lowercase().contains("line height"), "{text}");
        }
        // The sample is in the family the specimen draws, and a font row left
        // to discovery is never read out as a family.
        let family = SettingsPreviewSpec::typography(12.0)
            .with_font_candidate(SemanticFontCandidate {
                regular: Some("Berkeley Mono".to_string()),
                ..SemanticFontCandidate::default()
            })
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Berkeley Mono");
        assert!(family.semantic_value().contains("Berkeley Mono"));
        let discovered = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY_BOLD, "auto (discovered)")
            .with_focus_label("Bold font family");
        for text in [discovered.candidate_badge(), discovered.semantic_value()] {
            assert!(!text.contains("discovered"), "{text}");
        }
    }

    fn unpack(color: u32) -> [u8; 3] {
        let rgb = packed_rgb(color);
        [rgb.r, rgb.g, rgb.b]
    }

    #[test]
    fn spoken_palette_names_the_theme_its_side_and_measured_contrast() {
        let identity = PreviewThemeIdentity {
            label: "Arctic".to_string(),
            light: false,
        };
        let text = test_contrast_ratio(unpack(NORD.fg), unpack(NORD.bg));
        let illegible = NORD
            .ansi
            .iter()
            .filter(|&&color| test_contrast_ratio(unpack(color), unpack(NORD.bg)) < 3.0)
            .count();
        assert!(illegible > 0, "fixture has a dim slot");
        let words = if illegible == 1 { "colour" } else { "colours" };
        assert_eq!(
            spoken_palette(NORD, Some(&identity)),
            format!(
                "Arctic, dark, text contrast {}:1, {illegible} {words} below 3:1",
                trim_contrast(f64::from(text))
            )
        );
        // The identity's side is spoken, not re-measured.
        let light = PreviewThemeIdentity {
            light: true,
            ..identity
        };
        assert!(spoken_palette(NORD, Some(&light)).starts_with("Arctic, light,"));
    }

    #[test]
    fn spoken_palette_counts_exactly_the_ansi_slots_below_three_to_one() {
        let legible = PreviewTerminalTheme {
            ansi: [NORD.fg; 16],
            ..NORD
        };
        assert!(spoken_palette(legible, None).ends_with(", 0 colours below 3:1"));
        let invisible = PreviewTerminalTheme {
            ansi: [NORD.bg; 16],
            ..NORD
        };
        assert!(spoken_palette(invisible, None).ends_with(", 16 colours below 3:1"));
        let mut one = legible;
        one.ansi[5] = NORD.bg;
        assert!(spoken_palette(one, None).ends_with(", 1 colour below 3:1"));
        // With no identity the side is measured from the background.
        let paper = PreviewTerminalTheme {
            fg: 0x20_2020,
            bg: 0xFA_FAFA,
            ..NORD
        };
        assert!(spoken_palette(paper, None).contains(", light, "));
        assert!(spoken_palette(NORD, None).contains(", dark, "));
    }

    /// The spoken value names the theme in the painted badge's words, and
    /// says "sample" only for a card the stage draws; the badge says the
    /// still colour sample once, through its `Theme:`/`Sample:` subject.
    #[test]
    fn theme_stage_speaks_and_badges_the_theme_as_painted() {
        let theme = |value: &str, sample: bool| {
            SettingsPreviewSpec::appearance(14.0)
                .with_terminal_theme(NORD)
                .with_terminal_theme_identity(PreviewThemeIdentity {
                    label: "Default".to_string(),
                    light: false,
                })
                .with_focus(crate::prefs::EDIT_THEME, value)
                .with_theme_sample(sample)
        };
        let unset = crate::prefs::unset_theme_label("Default");
        let configured = theme(&unset, false);
        let badge = configured.with_state_badge(configured.candidate_badge());
        assert_eq!(badge, format!("Theme: {unset}"));
        let spoken = configured.semantic_value();
        assert!(
            spoken.starts_with(&format!("Color theme: {unset}, dark,")),
            "{spoken}"
        );
        assert!(!spoken.to_lowercase().contains("sample"), "{spoken}");

        let hearth = theme("Hearth", true);
        assert_eq!(
            hearth.with_state_badge(hearth.candidate_badge()),
            "Sample: Hearth"
        );
        let spoken = hearth.semantic_value();
        assert!(
            spoken.starts_with("Color theme sample: Hearth, dark,"),
            "{spoken}"
        );

        // The control: a setting that is not the theme keeps the state badge.
        let window = theme(&unset, false).with_focus(crate::prefs::EDIT_WINDOW_THEME, "light");
        let badge = window.with_state_badge(window.candidate_badge());
        assert!(
            badge.ends_with(&format!(" · {}", window.state_badge())),
            "{badge}"
        );
    }

    #[test]
    fn trim_contrast_never_rounds_a_ratio_up_to_its_floor() {
        assert_eq!(trim_contrast(2.96), "2.9");
        assert_eq!(trim_contrast(3.0), "3");
        assert_eq!(trim_contrast(21.0), "21");
        assert_eq!(trim_contrast(11.24), "11.2");
    }

    #[test]
    fn a_spec_without_a_candidate_draws_the_engine_default_ansi() {
        let host = Theme::default();
        let fallback = PreviewTerminalTheme::over_engine_palette(host);
        assert_eq!(
            fallback.ansi,
            PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()).ansi,
            "the specimen's engine table and the Default scheme agree"
        );
        assert_eq!(
            (fallback.fg, fallback.bg),
            (host.fg & 0xff_ffff, host.bg & 0xff_ffff)
        );
    }

    #[test]
    fn candidate_theme_changes_only_terminal_paint_and_declared_semantics() {
        let host = SettingsPreviewSpec::appearance(14.0);
        let nord = host.clone().with_terminal_theme(NORD);

        assert!(differences(&pixels(host.clone()), &pixels(nord.clone())) > 120);
        assert_ne!(host.paint_fingerprint(), nord.paint_fingerprint());

        let host_ui = compiled(host);
        let nord_ui = compiled(nord);
        assert_eq!(host_ui.bounds, nord_ui.bounds);
        assert_eq!(host_ui.hits, nord_ui.hits);
        assert_eq!(host_ui.focus_order, nord_ui.focus_order);

        let key = crate::native_ui::UiKey::new("preview");
        let host_semantic = host_ui.semantic(&key).expect("host preview semantics");
        let nord_semantic = nord_ui.semantic(&key).expect("Nord preview semantics");
        assert_eq!(host_semantic.key, nord_semantic.key);
        assert_eq!(host_semantic.parent, nord_semantic.parent);
        assert_eq!(host_semantic.rect, nord_semantic.rect);
        assert_eq!(host_semantic.role, nord_semantic.role);
        assert_eq!(host_semantic.label, nord_semantic.label);
        assert_eq!(host_semantic.state, nord_semantic.state);
        assert_eq!(host_semantic.action, nord_semantic.action);
        assert_eq!(host_semantic.audit_id, nord_semantic.audit_id);
        assert_ne!(host_semantic.value, nord_semantic.value);
        let crate::native_ui::SemanticValue::Text(value) = &nord_semantic.value else {
            panic!("preview exposes its candidate theme as text")
        };
        assert!(value.contains(&spoken_palette(NORD, None)));

        let host_prims = host_ui.tray(Theme::default(), 13.0).prims;
        let nord_prims = nord_ui.tray(Theme::default(), 13.0).prims;
        assert_eq!(
            panel_fill(&host_prims, 11.0),
            panel_fill(&nord_prims, 11.0),
            "candidate colors must not leak into host Settings chrome"
        );
        assert_ne!(panel_fill(&host_prims, 8.0), panel_fill(&nord_prims, 8.0));
        assert_eq!(
            panel_fill(&nord_prims, 8.0),
            rgba(crate::settings::u32_rgb(NORD.bg), 255)
        );
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn automatic_window_samples_follow_the_os_not_the_terminal_palette() {
        // Deliberately pair a dark terminal palette with OS Light, then flip
        // only the host appearance. Auto must repaint both preview scenes.
        let appearance_light = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_system_dark(false);
        let appearance_dark = appearance_light.clone().with_system_dark(true);
        assert!(
            appearance_light
                .semantic_value()
                .contains("currently Light"),
            "Auto's accessible value announces the resolved system appearance"
        );
        assert!(
            appearance_dark.semantic_value().contains("currently Dark"),
            "Auto's accessible value follows a system appearance change"
        );
        assert!(
            differences(
                &pixels(appearance_light.clone()),
                &pixels(appearance_dark.clone())
            ) > 40
        );
        assert_ne!(
            appearance_light.paint_fingerprint(),
            appearance_dark.paint_fingerprint()
        );

        // An explicit appearance is independent of the OS side of Auto.
        let explicit = appearance_light.with_appearance(AppearancePreviewSpec {
            window_theme: "dark".to_string(),
            ..AppearancePreviewSpec::default()
        });
        let explicit_flip = explicit.clone().with_system_dark(true);
        // The flipped paint recomputes: the two specs resolve to one theme, so
        // a plain second paint would be served the first one's specimen frame
        // (and its engine snapshot) and prove nothing about whether the OS side
        // of Auto reached the specimen at all.
        assert_eq!(
            differences(
                &pixels(explicit.clone()),
                &pixels_recomputed(explicit_flip.clone())
            ),
            0
        );
        assert_eq!(
            explicit.paint_fingerprint(),
            explicit_flip.paint_fingerprint()
        );
    }

    /// The Themes stage frames each theme in its own side: under Auto its
    /// window sample follows the candidate palette whatever the OS says, so
    /// a light theme previewed on a dark desktop is not drawn under a dark
    /// titlebar. A forced `window_theme` still wins.
    #[test]
    fn the_themes_stage_window_sample_takes_the_candidate_side() {
        const PAPER: PreviewTerminalTheme = PreviewTerminalTheme {
            fg: 0x33_3333,
            bg: 0xFA_FAFA,
            cursor: 0x35_84E4,
            selection: 0xD0_D7DE,
            ..NORD
        };
        let host = Theme::default();
        for system_dark in [false, true] {
            let stage = SettingsPreviewSpec::appearance(14.0)
                .with_system_dark(system_dark)
                .with_window_following_theme(true);
            let light = stage.clone().with_terminal_theme(PAPER);
            let dark = stage.clone().with_terminal_theme(NORD);
            assert!(light.window_sample_is_light(host), "os dark {system_dark}");
            assert!(!dark.window_sample_is_light(host), "os dark {system_dark}");
            assert!(light.semantic_value().contains("currently Light"));
            let forced = light.with_appearance(AppearancePreviewSpec {
                window_theme: "dark".to_string(),
                ..AppearancePreviewSpec::default()
            });
            assert!(!forced.window_sample_is_light(host));
        }
        // Negative control: off the stage, macOS and Windows Auto follow the
        // OS, so a light palette on a dark desktop draws a dark sample.
        let page = SettingsPreviewSpec::appearance(14.0)
            .with_system_dark(true)
            .with_terminal_theme(PAPER);
        assert_eq!(page.window_sample_is_light(host), cfg!(target_os = "linux"));
        let stage = page.clone().with_window_following_theme(true);
        if !cfg!(target_os = "linux") {
            assert_ne!(page.paint_fingerprint(), stage.paint_fingerprint());
            assert!(differences(&pixels(page), &pixels_recomputed(stage)) > 40);
        }
    }

    /// The Linux twin of the OS-following test above: winit's Wayland backend
    /// never delivers `ThemeChanged`, so the shipping chrome resolves Auto from
    /// the TERMINAL THEME's darkness (`chrome_theme_for_apprt` /
    /// `chrome_palette_theme`) — and the window sample must tell that truth:
    /// an OS appearance flip moves nothing, a terminal palette flip moves the
    /// sample titlebar.
    #[test]
    #[cfg(target_os = "linux")]
    fn automatic_window_samples_follow_the_terminal_palette_on_linux() {
        // A light candidate palette, luminance-opposite to NORD.
        let paper = PreviewTerminalTheme {
            fg: 0x33_3333,
            bg: 0xFA_FAFA,
            cursor: 0x35_84E4,
            selection: 0xD0_D7DE,
            ..NORD
        };
        let dark_palette = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_system_dark(false);
        assert!(
            dark_palette
                .semantic_value()
                .contains("matches the terminal theme, currently Dark"),
            "Auto's accessible value announces the side the terminal theme resolves to"
        );
        // Flip ONLY the host OS appearance: Auto ignores it here — the pixels
        // must not move (recomputed, so the check cannot be served the first
        // paint's cached specimen and prove nothing).
        let os_flip = dark_palette.clone().with_system_dark(true);
        assert!(
            os_flip
                .semantic_value()
                .contains("matches the terminal theme, currently Dark"),
            "an OS appearance flip does not move Linux Auto"
        );
        assert_eq!(
            differences(&pixels(dark_palette.clone()), &pixels_recomputed(os_flip)),
            0
        );
        // Flip the TERMINAL palette: Auto follows it.
        let light_palette = dark_palette.clone().with_terminal_theme(paper);
        assert!(
            light_palette
                .semantic_value()
                .contains("matches the terminal theme, currently Light"),
            "Auto's accessible value follows a terminal palette change"
        );
        assert!(differences(&pixels(dark_palette.clone()), &pixels(light_palette)) > 40);

        // An explicit appearance remains independent of both feeds.
        let explicit = dark_palette.with_appearance(AppearancePreviewSpec {
            window_theme: "dark".to_string(),
            ..AppearancePreviewSpec::default()
        });
        let explicit_flip = explicit.clone().with_system_dark(true);
        assert_eq!(
            differences(
                &pixels(explicit.clone()),
                &pixels_recomputed(explicit_flip.clone())
            ),
            0
        );
        assert_eq!(
            explicit.paint_fingerprint(),
            explicit_flip.paint_fingerprint()
        );
    }

    #[test]
    fn small_top_theme_card_always_shows_text_and_the_candidate_ansi_palette() {
        let candidate = PreviewTerminalTheme {
            ansi: [
                0x101010, 0xAA0000, 0x00AA00, 0xAAAA00, 0x0000AA, 0xAA00AA, 0x00AAAA, 0xAAAAAA,
                0x303030, 0xFF3030, 0x30FF30, 0xFFFF30, 0x3030FF, 0xFF30FF, 0x30FFFF, 0xFFFFFF,
            ],
            ..NORD
        };
        let spec = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(candidate)
            .with_focus(crate::prefs::EDIT_THEME, "Nord");
        let rect = LogicalRect::new(0.0, 0.0, 360.0, 132.0);
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            rect,
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        assert!(prims.iter().any(|primitive| matches!(
            primitive,
            DrawPrim::Text { s, face: TextFace::Mono, .. } if s == "$ aterm"
        )));
        let swatches = prims
            .iter()
            .filter_map(|primitive| match primitive {
                DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    fill,
                    ..
                } if (*radius - 2.0).abs() < f32::EPSILON && (*h - 6.0).abs() < f32::EPSILON => {
                    Some((*x, *y, *w, *fill))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(swatches.len(), 16, "all sixteen ANSI colours are visible");
        for (index, (x, y, width, fill)) in swatches.iter().enumerate() {
            assert_eq!(
                *fill,
                rgba(crate::settings::u32_rgb(candidate.ansi[index]), 255)
            );
            assert!(*x >= rect.x && *x + *width <= rect.right() + 0.01);
            assert!(*y >= rect.y && *y + 6.0 <= rect.bottom() + 0.01);
        }
    }

    #[test]
    fn color_specimen_keeps_its_authored_size_and_shows_fewer_whole_rows() {
        // Install and capture the same prepared renderer source a live native
        // Settings view receives. The fallback label belongs only to the
        // unavailable first frame and must not paint over a real specimen.
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let paint = |spec: &SettingsPreviewSpec, rect: LogicalRect| {
            let mut prims = Vec::new();
            spec.paint(
                &mut prims,
                rect,
                Theme::default(),
                Roles::from_theme(Theme::default()),
            );
            prims
        };
        let swatches = |prims: &[DrawPrim], swatch_y: f32| {
            prims
                .iter()
                .filter(|primitive| {
                    matches!(
                        primitive,
                        DrawPrim::Panel { y, h, .. }
                            if (*y - swatch_y).abs() < 0.01
                                && (*h - APPEARANCE_SWATCH_HEIGHT).abs() < 0.01
                    )
                })
                .count()
        };
        for font_px in [12.0, 24.0] {
            let spec =
                SettingsPreviewSpec::appearance(font_px).with_prepared_font(prepared.clone());
            let row = spec.normalized().typography_row_height();
            let floor = spec.minimum_height(48.0);
            let mut shown = Vec::new();
            for height in [floor, 96.0_f32.max(floor), 132.0_f32.max(floor)] {
                let rect = LogicalRect::new(0.0, 0.0, 360.0, height);
                let prims = paint(&spec, rect);
                let specimen = prims
                    .iter()
                    .find_map(|primitive| match primitive {
                        DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.as_ref()),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{font_px}px at {height}pt paints its specimen"));
                // A short card shows fewer rows; it never shrinks the type.
                assert_eq!(specimen.font_px, font_px, "{font_px}px at {height}pt");
                let terminal = LogicalRect::new(10.0, 32.0, 340.0, height - 42.0);
                let specimen_clip = appearance_specimen_clip(terminal);
                let rows = specimen.input.cells.len();
                assert!(rows >= 1, "{font_px}px at {height}pt keeps a whole row");
                assert_eq!(
                    rows,
                    ((specimen_clip.height / row).floor() as usize).min(APPEARANCE_SPECIMEN_ROWS)
                );
                assert!(rows as f32 * row <= specimen_clip.height + 0.01);
                let first: String = specimen.input.cells[0].iter().map(|cell| cell.ch).collect();
                assert!(first.starts_with("ANSI 4"), "the first rows stay: {first}");
                shown.push(rows);

                let swatch_y = appearance_swatch_y(terminal);
                assert!(specimen_clip.bottom() + APPEARANCE_SPECIMEN_SWATCH_GAP <= swatch_y + 0.01);
                assert!(swatch_y + APPEARANCE_SWATCH_HEIGHT <= terminal.bottom() + 0.01);
                assert_eq!(
                    swatches(&prims, swatch_y),
                    16,
                    "the whole palette band shows"
                );
                assert!(prims.iter().all(|primitive| !matches!(
                    primitive,
                    DrawPrim::Text { s, .. } if s == "$ aterm" || s == "ready"
                )));
                assert!(prims.iter().any(|primitive| matches!(
                    primitive,
                    DrawPrim::ClipPush { x, y, w, h }
                        if (*x - specimen_clip.x).abs() < 0.01
                            && (*y - specimen_clip.y).abs() < 0.01
                            && (*w - specimen_clip.width).abs() < 0.01
                            && (*h - specimen_clip.height).abs() < 0.01
                )));
            }
            assert!(
                shown.windows(2).all(|pair| pair[0] <= pair[1]),
                "a taller card never shows fewer rows: {shown:?}"
            );
            // Negative control: under its floor the card is its header alone,
            // never a strip of cut glyphs or a sliver of the palette.
            let sliver = paint(&spec, LogicalRect::new(0.0, 0.0, 360.0, floor - 1.0));
            assert!(sliver.iter().all(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { .. } => false,
                DrawPrim::Panel { h, radius, .. } => {
                    (*h - APPEARANCE_SWATCH_HEIGHT).abs() > 0.01 && (*radius - 8.0).abs() > 0.01
                }
                _ => true,
            }));
        }
        // The shipped 12px size keeps one whole row in the 72pt Top card.
        assert!(
            SettingsPreviewSpec::appearance(12.0)
                .with_prepared_font(prepared)
                .minimum_height(48.0)
                <= 72.0
        );
    }

    #[test]
    fn typography_sample_is_unicode_real_and_column_safe() {
        let prims = compiled(SettingsPreviewSpec::typography(14.0))
            .tray(Theme::default(), 13.0)
            .prims;
        let specimen = prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec),
                _ => None,
            })
            .expect("real terminal specimen primitive");
        let cells = specimen.input.cells.iter().flatten().collect::<Vec<_>>();
        assert!(cells.iter().any(|cell| cell.bold && !cell.italic));
        assert!(cells.iter().any(|cell| !cell.bold && cell.italic));
        assert!(cells.iter().any(|cell| cell.bold && cell.italic));
        assert!(
            cells
                .iter()
                .any(|cell| cell.underline != aterm_core::terminal::UnderlineStyle::None)
        );
        let terminal_text = cells.iter().map(|cell| cell.ch).collect::<String>();
        for sample in ['你', '好', '∑', '✓', '♥', '🚀', '😀', '👩'] {
            assert!(
                terminal_text.contains(sample),
                "missing fallback sample {sample}"
            );
        }
        assert_eq!(specimen.input.cursor_row, 1);
        // The caret follows the word `cursor`, leaving the ligature run clear.
        let row: String = specimen.input.cells[1].iter().map(|cell| cell.ch).collect();
        assert!(row[..specimen.input.cursor_col].ends_with("cursor"));
        assert!(specimen.input.selection.has_selection());
        // Trying Break ligatures at cursor puts the caret inside `!=`.
        let breaking = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_CURSOR_BREAK_LIGATURES, "true");
        let (input, _) = build_terminal_specimen_input(&breaking.normalized(), Theme::default());
        assert_eq!(input.cursor_col, 1);

        let mixed = "A你🚀B";
        assert_eq!(terminal_prefix(mixed, 0), "");
        assert_eq!(terminal_prefix(mixed, 2), "A");
        assert_eq!(terminal_prefix(mixed, 3), "A你");
        assert_eq!(terminal_prefix(mixed, 5), "A你🚀");

        let zwj = "A🐈‍⬛B";
        assert_eq!(terminal_prefix(zwj, 2), "A");
        assert_eq!(terminal_prefix(zwj, 3), "A🐈‍⬛");
        assert_eq!(grapheme_at_terminal_column("CJK: 你好", 5), Some("你"));
        assert_eq!(grapheme_at_terminal_column("CJK: 你好", 6), Some("你"));
    }

    #[test]
    fn visual_settings_change_real_pixels_without_changing_semantic_bounds() {
        let base = SettingsPreviewSpec::default().with_phase(780);
        let style = SettingsPreviewSpec {
            cursor: CursorPreviewSpec {
                trail_style: PreviewTrailStyle::Water,
                ..base.cursor.clone()
            },
            ..base.clone()
        };
        let font = SettingsPreviewSpec {
            font_px: 22.0,
            ..base.clone()
        };
        let reduced = base.clone().with_reduced_motion(true);

        let base_pixels = pixels(base.clone());
        assert!(differences(&base_pixels, &pixels(style.clone())) > 120);
        assert!(differences(&base_pixels, &pixels(font.clone())) > 120);
        assert!(differences(&base_pixels, &pixels(reduced.clone())) > 120);

        let bounds = |spec| {
            let compiled = compiled(spec);
            let node = compiled
                .semantic(&crate::native_ui::UiKey::new("preview"))
                .expect("semantic preview");
            assert_eq!(node.role, SemanticRole::Group);
            assert!(node.label.contains("preview"));
            assert!(matches!(
                node.value,
                crate::native_ui::SemanticValue::Text(_)
            ));
            assert!(
                compiled.hits.is_empty(),
                "a demonstration is not a fake control"
            );
            node.rect
        };
        assert_eq!(bounds(base.clone()), bounds(style));
        assert_eq!(bounds(base.clone()), bounds(font));
        assert_eq!(bounds(base), bounds(reduced));
    }

    #[test]
    fn injected_phase_is_deterministic_and_reduced_motion_is_stable() {
        let first = SettingsPreviewSpec::default().with_phase(420);
        let later = SettingsPreviewSpec::default().with_phase(980);
        assert_eq!(pixels(first.clone()), pixels(first));
        assert!(
            differences(
                &pixels(later.clone()),
                &pixels(SettingsPreviewSpec::default().with_phase(420))
            ) > 80
        );

        let reduced_first = SettingsPreviewSpec::default()
            .with_phase(420)
            .with_reduced_motion(true);
        let reduced_later = later.with_reduced_motion(true);
        assert_eq!(
            pixels(reduced_first),
            pixels(reduced_later),
            "reduced motion freezes both the trail and cursor blink"
        );

        let live_first = compiled(SettingsPreviewSpec::default().with_phase(420));
        let live_later = compiled(SettingsPreviewSpec::default().with_phase(980));
        assert_ne!(live_first.fingerprint(), live_later.fingerprint());
        let reduced_first = compiled(
            SettingsPreviewSpec::default()
                .with_phase(420)
                .with_reduced_motion(true),
        );
        let reduced_later = compiled(
            SettingsPreviewSpec::default()
                .with_phase(980)
                .with_reduced_motion(true),
        );
        assert_eq!(reduced_first.fingerprint(), reduced_later.fingerprint());
    }

    #[test]
    fn non_finite_glow_inputs_fail_off_like_the_live_renderer() {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Comet,
            duration_ms: u64::MAX,
            length: usize::MAX,
            intensity: f32::INFINITY,
            radius: f32::NAN,
            // The typing glow has no trail knob; this is about the trail's.
            momentum_glow: false,
            ..CursorPreviewSpec::default()
        })
        .with_phase(900)
        .normalized();
        assert_eq!(spec.cursor.duration_ms, MAX_DURATION_MS);
        assert_eq!(spec.cursor.length, MAX_LENGTH);
        assert_eq!(spec.cursor.intensity, 0.0);
        assert_eq!(spec.cursor.radius, 0.0);

        assert!(!has_effect_light(&played(&spec, Theme::default())));
    }

    fn rainbow_cursor(raw: &'static str) -> CursorPreviewSpec {
        CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::RainbowKitty,
            trail_style_raw: Arc::from(raw),
            ..CursorPreviewSpec::default()
        }
    }

    #[test]
    fn rainbow_preview_preserves_underline_and_tall_geometry_identity() {
        let theme = Theme::default();
        let underline = rainbow_cursor("rainbow kitty underline");
        let tall = rainbow_cursor("rainbow kitty tall");

        assert!(!underline.glow_config(theme, 0.5).ribbon_tall);
        assert!(tall.glow_config(theme, 0.5).ribbon_tall);
        assert_eq!(
            underline.effective_trail_style_raw(),
            "rainbow kitty underline"
        );
        assert_eq!(tall.effective_trail_style_raw(), "rainbow kitty tall");
        assert_ne!(
            SettingsPreviewSpec::cursor(underline).paint_fingerprint(),
            SettingsPreviewSpec::cursor(tall).paint_fingerprint(),
            "geometry variants cannot alias in the retained preview cache"
        );
    }

    #[test]
    fn preview_cache_identity_preserves_case_visible_style_text() {
        let lower = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying"));
        let authored = SettingsPreviewSpec::cursor(rainbow_cursor("Rainbow Kitty Flying"));
        assert_ne!(
            lower.paint_fingerprint(),
            authored.paint_fingerprint(),
            "case-visible badge text cannot alias in the retained preview cache"
        );
    }

    #[test]
    fn rainbow_preview_preserves_pet_dog_and_flying_companion_identity() {
        use aterm_effects::kitty_pet::PetSpecies;
        use std::hash::{Hash, Hasher};

        for (raw, expected) in [
            (
                "rainbow kitty pet",
                PreviewTrailCompanion::Pet(PetSpecies::Cat),
            ),
            (
                "rainbow dog pet",
                PreviewTrailCompanion::Pet(PetSpecies::Dog),
            ),
            ("rainbow kitty flying", PreviewTrailCompanion::FlyingKitty),
        ] {
            let cursor = rainbow_cursor(raw);
            assert_eq!(cursor.trail_companion(), expected, "{raw}");
            assert_eq!(cursor.effective_trail_style_raw(), raw, "{raw}");
        }

        // The same frame of each species: different texels under the body.
        let body = |raw: &'static str| {
            let input = played(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(900),
                Theme::default(),
            );
            let sprite = *input.free_sprites.first().expect("a drawn companion");
            let atlas = input.free_atlas.expect("its atlas");
            let width = atlas.width as usize;
            let texels = (0..usize::from(sprite.ah))
                .flat_map(|y| {
                    let start = ((usize::from(sprite.ay) + y) * width + usize::from(sprite.ax)) * 4;
                    atlas.rgba[start..start + usize::from(sprite.aw) * 4].to_vec()
                })
                .collect::<Vec<_>>();
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            texels.hash(&mut hash);
            hash.finish()
        };
        assert_ne!(
            body("rainbow kitty pet"),
            body("rainbow dog pet"),
            "species must change pixels"
        );
    }

    /// The cursor scene draws one caret: the specimen's own, at the cell the
    /// scripted typing has reached on the prompt — nothing painted over it.
    #[test]
    fn the_cursor_scene_draws_one_caret_on_the_typed_line() {
        for style in [PreviewCursorStyle::Block, PreviewCursorStyle::Bar] {
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                style,
                blink: false,
                ..CursorPreviewSpec::default()
            })
            .with_phase(900);
            let mut prims = Vec::new();
            spec.paint(
                &mut prims,
                LogicalRect::new(0.0, 0.0, 480.0, 156.0),
                Theme::default(),
                Roles::from_theme(Theme::default()),
            );
            assert!(
                prims.iter().all(|primitive| !matches!(
                    primitive,
                    DrawPrim::Panel { radius, .. } if *radius != 11.0 && *radius != 8.0
                )),
                "only the card and the terminal canvas are panels"
            );
            let (base, _) = build_terminal_specimen_input(&spec.normalized(), Theme::default());
            assert!(
                !base.cursor_visible,
                "the snapshot carries no caret of its own"
            );
            let input = played(&spec, Theme::default());
            assert!(input.cursor_visible);
            assert_eq!(
                input.cursor_row,
                input.rows - 2,
                "{style:?} types on the prompt above the free row"
            );
            let row = &input.cells[input.cursor_row];
            assert_eq!(
                row[input.cursor_col].ch, ' ',
                "the caret leads the typed text"
            );
            assert_ne!(
                row[input.cursor_col - 1].ch,
                ' ',
                "right after the last typed key"
            );
        }
    }

    /// The companion rides with the caret: on the caret's row, starting just
    /// past it, never climbing more than the one row above the prompt. (The
    /// Flying kitty from the moment it has flown in.)
    #[test]
    fn the_companion_rides_beside_the_caret_on_its_row() {
        let (cell_w, cell_h) = (8_i32, 17_i32);
        for raw in [
            "rainbow kitty pet",
            "rainbow dog pet",
            "rainbow kitty flying",
        ] {
            let phases: &[u64] = if raw.ends_with("flying") {
                &[1_500, 2_300]
            } else {
                &[0, 700, 1_500, 2_300]
            };
            for &phase in phases {
                let input = played(
                    &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(phase),
                    Theme::default(),
                );
                let row = input.cursor_row as i32;
                assert_eq!(
                    input.cursor_row,
                    prompt_row(&input),
                    "the caret types on the prompt"
                );
                let caret_x = input.cursor_col as i32 * cell_w;
                let sprite = input
                    .free_sprites
                    .iter()
                    .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                    .unwrap_or_else(|| panic!("{raw} at {phase} ms draws its companion"));
                assert!(
                    sprite.x >= caret_x && sprite.x <= caret_x + 2 * cell_w,
                    "{raw} at {phase} ms starts {} px from the caret",
                    sprite.x - caret_x
                );
                // A pet stands on the caret's row; the flying head may dip
                // into the free row below it.
                let floor = if raw.ends_with("flying") {
                    row + 2
                } else {
                    row + 1
                };
                assert!(
                    sprite.y >= (row - 1) * cell_h
                        && sprite.y + i32::from(sprite.h) <= floor * cell_h,
                    "{raw} at {phase} ms stays on the caret's row: y {}..{}",
                    sprite.y,
                    sprite.y + i32::from(sprite.h)
                );
            }
        }
    }

    #[test]
    fn reduced_motion_preview_keeps_resident_pets_but_suppresses_flight() {
        let reduced_input = |raw: &'static str| {
            played(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_reduced_motion(true),
                Theme::default(),
            )
        };

        for raw in ["rainbow kitty pet", "rainbow dog pet"] {
            let input = reduced_input(raw);
            assert!(
                !input.free_sprites.is_empty() && input.free_atlas.is_some(),
                "{raw} remains a static resident under Reduced Motion"
            );
        }

        let flying = reduced_input("rainbow kitty flying");
        assert!(
            flying.free_sprites.is_empty() && flying.free_atlas.is_none(),
            "ordinary flying-kitty motion stays suppressed"
        );

        let live_flying = played(
            &SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying")).with_phase(2_300),
            Theme::default(),
        );
        assert!(
            !live_flying.free_sprites.is_empty() && live_flying.free_atlas.is_some(),
            "negative control: the selected flying companion is drawable outside Reduced Motion"
        );
    }

    /// A ONE-ROW CARD HOLDS EVERY COMPANION WHOLE: a short window's preview
    /// shows only the prompt row, and a companion taller than a row (the
    /// flying head, and the resident cat and dog, 1.7 rows each) is drawn
    /// smaller inside it rather than cut at the card's edge, the pet on its
    /// legs at its station beside the caret, walking or sitting. NEGATIVE
    /// CONTROL: a card with room keeps each at its own size.
    #[test]
    fn a_one_row_card_holds_every_companion_whole() {
        let cell = (8, 17);
        for (raw, phases) in [
            ("rainbow kitty pet", &[0_u64, 700, 1_500, 2_300][..]),
            ("rainbow dog pet", &[0, 700, 1_500, 2_300][..]),
            ("rainbow kitty flying", &[2_300][..]),
        ] {
            for &phase in phases {
                let spec = SettingsPreviewSpec::cursor(rainbow_cursor(raw))
                    .with_phase(phase)
                    .normalized();
                let body = |rows: Option<usize>| {
                    let (base, _) =
                        build_terminal_specimen_input_rows(&spec, Theme::default(), rows);
                    let input = CursorScene::new(&spec, Theme::default()).decorate(&base, cell);
                    let sprite = input
                        .free_sprites
                        .iter()
                        .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                        .copied()
                        .unwrap_or_else(|| panic!("{raw} at {phase} ms is drawn"));
                    (input.rows, sprite)
                };
                let at = format!("{raw} at {phase} ms");
                let (rows, short) = body(Some(1));
                assert_eq!(rows, 1, "{at}");
                assert!(
                    short.y >= 0 && short.y + i32::from(short.h) <= 17,
                    "{at}: whole in one row: y {} h {}",
                    short.y,
                    short.h
                );
                assert!(short.h >= 16, "{at}: not shrunk to a speck: h {}", short.h);
                let (rows, roomy) = body(None);
                assert!(rows > 2, "{at}");
                assert!(
                    roomy.h > 17,
                    "{at}: with room, it keeps its own size: {}",
                    roomy.h
                );
                if raw.ends_with("pet") {
                    assert!(
                        (short.x - roomy.x).abs() <= 2,
                        "{at}: the pet keeps its station beside the caret: {} against {}",
                        short.x,
                        roomy.x
                    );
                }
            }
        }
    }

    /// THE FLYING KITTY IS EARNED, in the preview as in the terminal: the
    /// scripted typing shows the ribbon alone for its first keys and the
    /// kitty flying in partway through, never a kitty there from the first
    /// key. The sprite control's diagnostic kitty still shows at once, and a
    /// walking pet is there from the start.
    #[test]
    fn the_flying_kitty_arrives_partway_through_the_typing() {
        let at = |raw: &'static str, phase: u64| {
            played(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(phase),
                Theme::default(),
            )
        };
        for phase in [0, 700] {
            let early = at("rainbow kitty flying", phase);
            assert!(
                early.free_sprites.is_empty(),
                "no kitty yet at {phase} ms: {} sprites",
                early.free_sprites.len()
            );
            assert!(
                !at("rainbow kitty pet", phase).free_sprites.is_empty(),
                "negative control: the walking cat is there at {phase} ms"
            );
        }
        assert!(!at("rainbow kitty flying", 2_300).free_sprites.is_empty());
        let diagnostic = played(
            &SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying"))
                .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "")
                .with_phase(0),
            Theme::default(),
        );
        assert!(
            !diagnostic.free_sprites.is_empty(),
            "the sprite control previews its kitty at once"
        );
    }

    #[test]
    fn custom_pack_preview_uses_the_shared_engine_and_pack_identity() {
        let synthwave = compile_pack(include_str!(
            "../../aterm-effects/assets/trail-packs/synthwave.toml"
        ));
        let emberfall = compile_pack(include_str!(
            "../../aterm-effects/assets/trail-packs/emberfall.toml"
        ));
        let custom = |pack| {
            SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::Custom,
                trail_pack: Some(Arc::new(pack)),
                ..CursorPreviewSpec::default()
            })
            .with_phase(900)
        };
        let synthwave_spec = custom(synthwave);
        let emberfall_spec = custom(emberfall);

        assert_eq!(synthwave_spec.animation(), PreviewAnimation::Continuous);
        assert!(
            synthwave_spec
                .audit_value()
                .contains("shared CursorGlow custom Trail Pack interpreter is live")
        );
        assert_ne!(
            synthwave_spec.paint_fingerprint(),
            emberfall_spec.paint_fingerprint(),
            "the retained preview identity includes the selected pack"
        );
        assert!(
            differences(&pixels(synthwave_spec.clone()), &pixels(emberfall_spec)) > 80,
            "different packs must produce visibly different renderer output"
        );

        assert!(
            has_effect_light(&played(&synthwave_spec, Theme::default())),
            "a loaded custom pack reaches the real CursorGlow interpreter"
        );
    }

    #[test]
    fn resolved_custom_kitty_asset_is_drawn_even_while_the_style_is_dormant() {
        let rgba: Arc<[u8]> = Arc::from(
            [
                255, 40, 80, 255, 20, 240, 255, 255, 255, 220, 30, 255, 80, 30, 255, 255,
            ]
            .as_slice(),
        );
        let custom_asset = crate::app_config::KittySpriteAsset::Ready {
            source_id: Arc::from("test-cat.png"),
            w: 2,
            h: 2,
            rgba: Arc::clone(&rgba),
            fp: 0xCA7C_A7C0_1234_5678,
        };
        // Rainbow kitty is the DEFAULT trail style now, so pin the spec to phaser — the
        // dormant-rainbow kitty path this test exercises requires a non-rainbow-kitty style.
        let custom = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Phaser,
            ..CursorPreviewSpec::default()
        })
        .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "test-cat.png")
        .with_post_fx(CursorPostFxSpec {
            kitty_sprite: "test-cat.png".to_string(),
            kitty_asset: custom_asset,
            ..CursorPostFxSpec::default()
        });
        assert_eq!(custom.cursor.trail_style, PreviewTrailStyle::Phaser);
        assert!(custom.audit_value().contains("shown independently"));
        assert!(custom.audit_value().contains("dormant"));
        let crate::app_config::KittySpriteAsset::Ready { rgba: carried, .. } =
            &custom.post_fx.kitty_asset
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(carried, &rgba));

        let input = played(&custom, Theme::default());
        assert!(!input.free_sprites.is_empty());
        assert!(input.free_atlas.is_some());
        assert!(
            differences(
                &pixels(custom),
                &pixels(
                    SettingsPreviewSpec::cursor(CursorPreviewSpec {
                        trail_style: PreviewTrailStyle::Phaser,
                        ..CursorPreviewSpec::default()
                    })
                    .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "built-in CatBaker"),
                ),
            ) > 20,
            "chosen custom sprite changes exact specimen pixels",
        );
    }

    #[test]
    fn unavailable_custom_pack_fails_closed_without_invisible_animation() {
        let unavailable = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::Custom,
            trail_pack: None,
            ..CursorPreviewSpec::default()
        })
        .with_phase(900);

        assert_eq!(unavailable.animation(), PreviewAnimation::None);
        assert_eq!(unavailable.state_badge(), "Trail unavailable");
        assert!(unavailable.audit_value().contains("fails closed"));
        assert_eq!(
            unavailable.paint_fingerprint(),
            unavailable.clone().with_phase(1_500).paint_fingerprint(),
            "an unavailable pack cannot schedule phase-only rerasterization"
        );

        assert!(!has_effect_light(&played(&unavailable, Theme::default())));
    }

    /// A TYPO PREVIEWS THE DEFAULT, not an empty box: `resolve_trail_style`
    /// substitutes [`crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE`] for an
    /// unrecognized spelling, so the preview has to show the same trail — and
    /// the same COMPANION, which is the half that reads the style token rather
    /// than the resolved `GlowStyle`. The badge still quotes what was authored.
    #[test]
    fn unknown_style_previews_the_default_trail_and_companion() {
        let cursor = CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::parse("phasr"),
            trail_style_raw: Arc::from("phasr"),
            ..CursorPreviewSpec::default()
        };
        let default_cursor = rainbow_cursor(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
        assert_eq!(cursor.trail_style, default_cursor.trail_style);
        assert_eq!(cursor.trail_companion(), default_cursor.trail_companion());
        assert_eq!(
            cursor.effective_trail_style_token(),
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
        );
        assert_eq!(
            cursor.effective_trail_style_raw(),
            "phasr",
            "the badge keeps the authored spelling"
        );

        let spec = SettingsPreviewSpec::cursor(cursor).with_phase(900);
        assert_ne!(spec.animation(), PreviewAnimation::None);
        assert!(!spec.audit_value().contains("fails closed"));
        assert!(has_effect_light(&played(&spec, Theme::default())));
    }

    #[test]
    fn malformed_styles_fail_closed_instead_of_previewing_lumen() {
        for raw in ["pack:", "pack:missing"] {
            let style = PreviewTrailStyle::parse(raw);
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: style,
                trail_pack: None,
                ..CursorPreviewSpec::default()
            })
            .with_phase(900);
            assert_eq!(spec.animation(), PreviewAnimation::None, "{raw}");
            assert!(spec.audit_value().contains("fails closed"), "{raw}");
            assert!(!has_effect_light(&played(&spec, Theme::default())), "{raw}");
        }
    }

    #[test]
    fn audit_value_is_truthful_about_renderer_scope() {
        let value = SettingsPreviewSpec::default().audit_value();
        assert!(value.contains("renderer preview"));
        assert!(value.contains("host-prepared renderer snapshot unavailable"));
        assert!(value.contains("portable SDR tone-map: bloom"));
        assert!(value.contains("fire shimmer"));
        assert!(value.contains("no panel-headroom claim"));
        assert!(value.contains("SDR boost"));
        // Named from the SHIPPED default rather than spelled again here: the
        // preview's whole claim is that it describes what the product actually
        // does, so the token it prints has to be the one an unset
        // `cursor_trail_style` resolves to. Spelling it twice is how this
        // assertion came to outlive the default it was written for — the
        // companion joined the default and the literal stayed behind.
        assert!(
            value.contains(&format!(
                "{} trail",
                crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
            )),
            "the audit value must name the shipped default trail style: {value}"
        );

        let rainbow_kitty = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_style: PreviewTrailStyle::RainbowKitty,
            ..CursorPreviewSpec::default()
        })
        .audit_value();
        assert!(rainbow_kitty.contains("resident cat companion"));
        assert!(rainbow_kitty.contains("built-in CatBaker asset ready"));
        assert!(!rainbow_kitty.contains("not simulated"));

        let off = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_enabled: false,
            trail_style: PreviewTrailStyle::Off,
            ..CursorPreviewSpec::default()
        })
        .audit_value();
        assert!(off.contains("No CursorGlow trail primitive is emitted"));
        assert!(!off.contains("Nyan ribbon"));
    }

    #[test]
    fn accessibility_values_are_concise_user_language_while_audit_stays_detailed() {
        let specimens = [
            SettingsPreviewSpec::appearance(14.0)
                .with_terminal_theme(NORD)
                .with_focus(crate::prefs::EDIT_THEME, "Nord"),
            SettingsPreviewSpec::typography(16.0)
                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Berkeley Mono"),
            SettingsPreviewSpec::default(),
        ];
        for spec in specimens {
            let value = spec.semantic_value();
            assert!(value.len() < 240, "spoken preview is too long: {value}");
            for internal in [
                "normalized-candidate",
                "CursorGlow",
                "CatBaker",
                "tone-map",
                "fingerprint",
            ] {
                assert!(!value.contains(internal), "{internal} leaked into {value}");
            }
        }
        let audit = SettingsPreviewSpec::default().audit_value();
        assert!(audit.contains("normalized-candidate"));
        assert!(audit.contains("CursorGlow"));
        assert!(audit.contains("tone-map"));
    }

    #[test]
    fn unresolved_font_candidate_is_acknowledged_inside_the_specimen() {
        let family = "No Such Font Family 7F31";
        let spec = SettingsPreviewSpec::typography(14.0)
            .with_font_candidate(SemanticFontCandidate {
                regular: Some(family.to_string()),
                ..SemanticFontCandidate::default()
            })
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, family);
        let audit = spec.audit_value();
        let compiled = compiled(spec);
        let preview = compiled
            .semantic(&crate::native_ui::UiKey::new("preview"))
            .expect("preview semantics");
        let prims = compiled.tray(Theme::default(), 13.0).prims;
        let pill = prims.iter().find_map(|prim| match prim {
            DrawPrim::Text { baseline, s, .. } if s.contains("Regular:") && s.contains(family) => {
                Some((*baseline, s))
            }
            _ => None,
        });
        let (baseline, label) = pill.expect("candidate status capsule text");
        assert!(
            baseline > preview.rect.y + 33.0,
            "pill is inside the specimen body"
        );
        assert!(
            label.contains("Preparing") || label.contains("Unavailable"),
            "candidate is never misrepresented as live: {label}",
        );
        let crate::native_ui::SemanticValue::Text(value) = &preview.value else {
            panic!("preview semantic value is textual")
        };
        assert!(value.contains(family));
        assert!(audit.contains("In-specimen font status:"));
        assert!(audit.contains(family));
    }

    /// A font that failed is named by its own slot, whichever font row has
    /// focus: the emoji font, not the Regular the focus would suggest.
    #[test]
    fn a_failed_font_status_names_the_slot_that_failed() {
        let mut spec = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "")
            .with_font_candidate(SemanticFontCandidate {
                emoji: Some("NoSuchEmoji".to_string()),
                ..SemanticFontCandidate::default()
            });
        spec.font_status = "candidate fallback active: unresolved emoji \"NoSuchEmoji\": \
                            \"NoSuchEmoji\" does not resolve to a font file; requested emoji \
                            \"NoSuchEmoji\""
            .to_string();
        assert_eq!(
            spec.font_candidate_pill(),
            Some(("Not found · Emoji: NoSuchEmoji".to_string(), true))
        );
        // A family that is not found is not what the sample is said to show.
        let mut missing =
            SettingsPreviewSpec::typography(14.0).with_font_candidate(SemanticFontCandidate {
                regular: Some("NoSuchFontXYZ".to_string()),
                ..SemanticFontCandidate::default()
            });
        missing.font_status = "candidate fallback active: unresolved regular \"NoSuchFontXYZ\": \
                               \"NoSuchFontXYZ\" does not resolve to a font file; requested \
                               regular \"NoSuchFontXYZ\""
            .to_string();
        assert!(
            missing
                .semantic_value()
                .starts_with("Text sample in the current font, because \u{201c}NoSuchFontXYZ\u{201d} is not found"),
            "{}",
            missing.semantic_value()
        );
        // Nor is it what the header says the sample is in.
        let missing = missing.with_focus(crate::prefs::EDIT_FONT_FAMILY, "NoSuchFontXYZ");
        assert_eq!(missing.candidate_badge(), "Font: Current font");
        // A name that resolves is named.
        let mut found = missing.clone();
        found.font_status = "candidate renderer font cascade ready".to_string();
        assert_eq!(found.candidate_badge(), "Font: NoSuchFontXYZ");
    }

    #[test]
    fn a_ready_font_has_no_status_pill_and_a_loading_one_does_not_repeat_its_name() {
        let mut spec = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default");
        // This is the exact settled status emitted for the host candidate by
        // `SemanticFontResolution::Host`.  Keep the presentation assertion in
        // this module, beside the classifier that owns the warning capsule.
        // The header badge already names the font; the glyphs are the proof.
        spec.font_status = "committed renderer font cascade ready".to_string();
        assert_eq!(spec.font_candidate_pill(), None);
        assert!(spec.candidate_badge().contains("Automatic"));

        // While it loads, the pill says only that; the header names the font.
        spec.font_status = "candidate loading".to_string();
        assert_eq!(
            spec.font_candidate_pill(),
            Some(("Loading font".to_string(), false))
        );
    }

    #[test]
    fn typography_reserves_whole_candidate_rows_above_its_status_band() {
        // Load the real deterministic specimen renderer through the paint
        // fixture, then keep its immutable snapshot for every geometry probe.
        let _ = compiled(SettingsPreviewSpec::typography(12.0));
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        let theme = Theme::default();
        for (status, has_pill) in [
            ("committed renderer font cascade ready", false),
            ("candidate loading", true),
        ] {
            for width in [254.0, 592.0, 900.0] {
                for font_px in [12.0, 24.0, 36.0, 72.0] {
                    for line_height in [0.8, 1.0, 2.0] {
                        let mut spec = SettingsPreviewSpec::typography(font_px)
                            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
                            .with_prepared_font(prepared.clone());
                        spec.line_height = line_height;
                        spec.font_status = status.to_string();
                        let full_height = spec.preferred_height(108.0);
                        for height in [108.0, full_height] {
                            let rect = LogicalRect::new(0.0, 0.0, width, height);
                            let mut prims = Vec::new();
                            spec.paint(&mut prims, rect, theme, Roles::from_theme(theme));
                            let pill_y = prims.iter().find_map(|prim| match prim {
                                DrawPrim::Panel { y, radius, .. } if *radius == 6.0 => Some(*y),
                                _ => None,
                            });
                            let specimen = prims.iter().find_map(|prim| match prim {
                                DrawPrim::TerminalSpecimen { y, spec, .. } => Some((*y, spec)),
                                _ => None,
                            });
                            let body = height - preview_header_height(&spec) - 15.0;
                            let row = spec.normalized().typography_row_height();
                            // The status band yields only to a whole row of
                            // glyphs it would otherwise leave no room for; a
                            // body that holds neither is the header alone.
                            let Some(pill_y) = pill_y else {
                                if body - 4.0 >= row {
                                    assert!(
                                        specimen
                                            .is_some_and(|(_, specimen)| specimen.input.rows >= 1),
                                        "{width}px/{height}px: {font_px}px drops its status for no row"
                                    );
                                } else {
                                    assert!(specimen.is_none(), "no cut row under the header");
                                }
                                continue;
                            };
                            assert!(has_pill, "a ready font paints no status pill");
                            if height == full_height {
                                assert_eq!(
                                    specimen.as_ref().unwrap().1.input.rows,
                                    TYPOGRAPHY_SPECIMEN_ROWS,
                                    "all five intended rows fit {width}px / {font_px}px / {line_height}"
                                );
                            }
                            if let Some((y, specimen)) = specimen {
                                assert_eq!(
                                    specimen.font_px, font_px,
                                    "never shrink the authored type"
                                );
                                assert_eq!(specimen.line_height, line_height);
                                assert!(specimen.input.rows <= TYPOGRAPHY_SPECIMEN_ROWS);
                                let first: String =
                                    specimen.input.cells[0].iter().map(|cell| cell.ch).collect();
                                assert!(
                                    first.starts_with("ANSI 4"),
                                    "a short specimen keeps the first rows, not parser-scroll tail: {first}"
                                );
                                for dpi in [1.0, 1.25, 2.0, 3.0] {
                                    let row_h = prepared
                                        .specimen_cell_height(font_px * dpi, line_height, &[])
                                        .unwrap()
                                        as f32
                                        / dpi;
                                    assert!(
                                        y + specimen.input.rows as f32 * row_h <= pill_y - 4.0,
                                        "{width}px/{height}px: {font_px}px at {dpi}dpi overlaps the badge"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        // Historical negative control: the old compact card held five rows
        // at24px under a bottom pill, which physically cannot fit.
        let old_terminal_h = 108.0 - 27.0 - 15.0;
        let row_h = prepared.specimen_cell_height(48.0, 1.0, &[]).unwrap() as f32 / 2.0;
        assert!(row_h * TYPOGRAPHY_SPECIMEN_ROWS as f32 > old_terminal_h - 24.0);
    }

    /// A display face's capitals rise past its cell; the specimen reserves
    /// the band above its first row the frame draws them into, so the box
    /// never cuts their tops off, and still shows whole rows under it.
    #[test]
    fn a_display_face_specimen_keeps_room_for_its_capitals() {
        let family = "display:chunky";
        let candidate = SemanticFontCandidate {
            regular: Some(family.to_string()),
            ..SemanticFontCandidate::default()
        };
        let prepared = crate::tray_raster::prepared_semantic_font_from_bytes_for_test(
            &candidate,
            aterm_render::display_face_bytes("chunky").unwrap(),
        );
        let spec = SettingsPreviewSpec::typography(12.0)
            .with_font_candidate(candidate)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, family)
            .with_prepared_font(prepared);
        let head = spec.normalized().specimen_headroom();
        assert!(head > 0.0, "Luckiest Guy's capitals rise past its cell");
        let row = spec.normalized().typography_row_height();
        let theme = Theme::default();
        for rows in 1..=TYPOGRAPHY_SPECIMEN_ROWS {
            let height = spec.typography_height(rows);
            let mut prims = Vec::new();
            spec.paint(
                &mut prims,
                LogicalRect::new(0.0, 0.0, 600.0, height),
                theme,
                Roles::from_theme(theme),
            );
            let specimen = prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::TerminalSpecimen { spec, .. } => Some(spec),
                    _ => None,
                })
                .expect("a whole row fits");
            assert!(specimen.input.rows >= rows, "{rows} rows at {height}pt");
            let body = height - preview_header_height(&spec.normalized()) - 15.0;
            assert!(head + specimen.input.rows as f32 * row <= body + 0.01);
        }
        // An ordinary font reserves nothing.
        let plain = SettingsPreviewSpec::typography(12.0).with_prepared_font(
            crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                &SemanticFontCandidate::default(),
            ),
        );
        assert_eq!(plain.normalized().specimen_headroom(), 0.0);
    }

    /// A text preview squeezed under one whole row paints its header alone:
    /// no dark strip, and no status pill sliced through its label.
    #[test]
    fn a_sliver_text_preview_is_its_header_alone() {
        let _ = compiled(SettingsPreviewSpec::typography(12.0));
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        let theme = Theme::default();
        let mut spec = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
            .with_prepared_font(prepared);
        spec.font_status = "candidate loading".to_string();
        let paint = |height: f32| {
            let mut prims = Vec::new();
            spec.paint(
                &mut prims,
                LogicalRect::new(0.0, 0.0, 552.0, height),
                theme,
                Roles::from_theme(theme),
            );
            prims
        };
        let sliver = paint(48.0);
        assert!(sliver.iter().all(|prim| match prim {
            DrawPrim::TerminalSpecimen { .. } => false,
            DrawPrim::Panel { radius, .. } => *radius > 10.0,
            DrawPrim::Text { s, .. } => !s.contains("Loading font"),
            _ => true,
        }));
        // Negative control: at its floor it is a whole row and the status.
        let floor = paint(spec.minimum_height(48.0));
        assert!(
            floor
                .iter()
                .any(|prim| matches!(prim, DrawPrim::TerminalSpecimen { .. }))
        );
        assert!(floor.iter().any(|prim| matches!(
            prim,
            DrawPrim::Text { s, .. } if s == "Loading font"
        )));
    }

    #[test]
    fn exact_candidate_font_size_and_cursor_shapes_reach_the_draw_ir() {
        let exact = SettingsPreviewSpec::typography(21.5);
        let prims = compiled(exact).tray(Theme::default(), 13.0).prims;
        let sizes = prims
            .iter()
            .filter_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.font_px),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!sizes.is_empty());
        assert!(sizes.iter().all(|size| *size == 21.5));

        let minimum = compiled(SettingsPreviewSpec::typography(1.0))
            .tray(Theme::default(), 13.0)
            .prims
            .into_iter()
            .filter_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.font_px),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!minimum.is_empty());
        assert!(minimum.iter().all(|size| *size == 6.0));

        let shape = |style| {
            pixels(SettingsPreviewSpec::cursor(CursorPreviewSpec {
                style,
                blink: false,
                trail_enabled: false,
                trail_style: PreviewTrailStyle::Off,
                ..CursorPreviewSpec::default()
            }))
        };
        let block = shape(PreviewCursorStyle::Block);
        let bar = shape(PreviewCursorStyle::Bar);
        assert!(differences(&block, &bar) > 40);
    }

    /// Pins the fixture invariant behind every pixel-identity assertion in
    /// this module: the semantic font is SETTLED — no async prewarm is in
    /// flight after `compiled()`, so nothing can land between two `pixels()`
    /// calls and swap the cascade mid-test. (With the production
    /// `set_chrome_fonts` seam this raced: a landing between two paints of
    /// one spec changed ~1,000 pixel bytes, failing four tests in parallel
    /// suite runs while every isolated run passed.)
    ///
    /// The second paint deliberately goes through [`pixels_recomputed`]: a
    /// plain repeat paint now hits the specimen snapshot memo AND the retained
    /// specimen frame, which would satisfy the assertion below for the whole
    /// terminal specimen without re-deriving a single glyph. Only a paint that
    /// misses both can witness a cascade swap — or any other nondeterminism in
    /// a repeat render, such as lazily discovered fallback faces.
    #[test]
    fn fixture_semantic_font_is_settled_so_repeated_paints_are_identical() {
        let spec = SettingsPreviewSpec::appearance(14.0).with_phase(100);
        let before = pixels(spec.clone());
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(
            !prepared.snapshot.pending,
            "fixture semantic font must be settled, never async-pending: {}",
            prepared.snapshot.status
        );
        assert!(
            prepared.renderer_ready(),
            "the assertion must inspect the immutable renderer injected by compiled()"
        );
        let after = pixels_recomputed(spec);
        assert_eq!(
            differences(&before, &after),
            0,
            "repeated paints of one spec must be byte-identical"
        );
    }

    #[test]
    fn appearance_is_truthfully_static_at_every_phase() {
        let early = SettingsPreviewSpec::appearance(14.0).with_phase(100);
        let late = SettingsPreviewSpec::appearance(14.0).with_phase(1_700);
        assert_eq!(early.animation(), PreviewAnimation::None);
        assert_eq!(late.animation(), PreviewAnimation::None);
        assert_eq!(early.paint_fingerprint(), late.paint_fingerprint());
        assert_eq!(pixels(early.clone()), pixels(late));
        assert!(early.audit_value().contains("blink off"));
    }

    #[test]
    fn only_pixel_visible_subjects_request_animation() {
        let zero_intensity = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Phaser,
            intensity: 0.0,
            // The typing glow types at any trail intensity; this is about
            // the trail alone.
            momentum_glow: false,
            ..CursorPreviewSpec::default()
        });
        assert_eq!(zero_intensity.animation(), PreviewAnimation::None);
        assert_eq!(
            pixels(zero_intensity.clone().with_phase(100)),
            pixels(zero_intensity.with_phase(900)),
            "zero intensity must really be pixel-static, not only timer-static"
        );

        let moving = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Phaser,
            intensity: 0.05,
            ..CursorPreviewSpec::default()
        });
        assert_eq!(moving.animation(), PreviewAnimation::Continuous);
    }

    #[test]
    fn blink_arms_only_visual_edges_and_quantizes_paint_identity() {
        let blink = |phase_ms| {
            SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: true,
                trail_enabled: false,
                trail_style: PreviewTrailStyle::Off,
                ..CursorPreviewSpec::default()
            })
            .with_phase(phase_ms)
        };
        assert_eq!(
            blink(100).animation(),
            PreviewAnimation::BlinkEdge { after_ms: 430 }
        );
        assert_eq!(
            blink(529).animation(),
            PreviewAnimation::BlinkEdge { after_ms: 1 }
        );
        assert_eq!(
            blink(530).animation(),
            PreviewAnimation::BlinkEdge { after_ms: 530 }
        );
        assert_eq!(
            blink(2_200).animation(),
            PreviewAnimation::BlinkEdge { after_ms: 730 },
            "the 2,400 ms phase reset is not a visual blink edge"
        );

        assert_eq!(
            blink(100).paint_fingerprint(),
            blink(500).paint_fingerprint()
        );
        assert_eq!(pixels(blink(100)), pixels(blink(500)));
        assert_ne!(
            blink(500).paint_fingerprint(),
            blink(600).paint_fingerprint()
        );
        assert_ne!(pixels(blink(500)), pixels(blink(600)));
    }

    /// The preview feeds the effect engine the colours a window does
    /// (`cursor_fx::follow_live_cursor`): with no trail colour set, Comet,
    /// Beam and Sparkle draw in the theme's cursor colour, not a built-in
    /// hue; Laser keeps its violet; a set colour wins.
    #[test]
    fn preview_trail_colours_follow_the_cursor_like_a_window() {
        let theme = PreviewTerminalTheme {
            cursor: 0x50_FA7B,
            ..NORD
        };
        let drawn = |style: &'static str, color: Option<u32>| {
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::parse(style),
                trail_style_raw: Arc::from(style),
                color,
                ..CursorPreviewSpec::default()
            })
            .with_terminal_theme(theme)
            .with_phase(900);
            let lit = played(&spec, theme.into_theme());
            let mut colors = Vec::new();
            colors.extend(lit.cursor_fill_override);
            if !lit.cursor_trail.is_empty() {
                colors.push(lit.cursor_trail_color);
            }
            assert!(!colors.is_empty(), "{style} lights the caret or a trail");
            colors
        };
        let channels = |rgb: u32| ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        for style in ["comet", "beam", "sparkle"] {
            for rgb in drawn(style, None) {
                let (r, g, b) = channels(rgb);
                assert!(
                    g > b && g > r,
                    "{style} draws the green cursor colour: {rgb:06x}"
                );
            }
            // A set colour wins.
            for rgb in drawn(style, Some(0x00E0_218A)) {
                let (r, g, b) = channels(rgb);
                assert!(r > g && r > b, "{style} draws its set colour: {rgb:06x}");
            }
        }
        for rgb in drawn("laser", None) {
            let (r, g, b) = channels(rgb);
            assert!(b > g && r > g, "Laser keeps its violet: {rgb:06x}");
        }
    }

    /// Off previews what the terminal draws after choosing it: no trail, but
    /// the typing glow while it is on, so the scripted typing plays.
    #[test]
    fn the_off_preview_types_while_the_typing_glow_is_on() {
        let off = |momentum_glow: bool| {
            SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_enabled: true,
                trail_style: PreviewTrailStyle::Off,
                trail_style_raw: Arc::from("off"),
                momentum_glow,
                ..CursorPreviewSpec::default()
            })
        };
        assert_eq!(off(true).animation(), PreviewAnimation::Continuous);
        assert_eq!(off(true).state_badge(), "Live typing glow");
        assert!(off(true).semantic_value().contains("glowing as you type"));
        assert_eq!(off(false).animation(), PreviewAnimation::None);
        assert!(!off(false).semantic_value().contains("glowing"));
    }

    /// On a light terminal the typing glow has no halo (light cannot show
    /// on paper): Off's preview never says it glows there. A block caret's
    /// fill still warms, so it says that; a bar shows nothing, so it says
    /// no more than the typing.
    #[test]
    fn the_off_preview_promises_no_glow_on_a_light_terminal() {
        let off = |style: PreviewCursorStyle, bg: u32| {
            let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                style,
                blink: false,
                trail_enabled: true,
                trail_style: PreviewTrailStyle::Off,
                trail_style_raw: Arc::from("off"),
                momentum_glow: true,
                ..CursorPreviewSpec::default()
            });
            spec.terminal_theme = Some(PreviewTerminalTheme::new(
                0x0080_8080,
                bg,
                0x0009_69DA,
                0x0080_8080,
                [0; 16],
            ));
            spec
        };
        let dark = off(PreviewCursorStyle::Block, 0x0011_1111);
        assert_eq!(dark.state_badge(), "Live typing glow");
        assert!(dark.semantic_value().contains("glowing as you type"));
        let block = off(PreviewCursorStyle::Block, 0x00FF_FFFF);
        assert_eq!(block.animation(), PreviewAnimation::Continuous);
        assert_eq!(block.state_badge(), "Live typing tint");
        assert!(
            block
                .semantic_value()
                .contains("no trail, tinting as you type")
        );
        let bar = off(PreviewCursorStyle::Bar, 0x00FF_FFFF);
        assert_eq!(bar.state_badge(), "Live typing");
        assert!(!bar.semantic_value().contains("glow"));
        assert!(!bar.semantic_value().contains("tint"));
    }
}
