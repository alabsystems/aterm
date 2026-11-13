// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-native live demonstrations for Settings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_effects::cursor_fx::{CursorFx, CursorFxInput};
use aterm_render::{GlowQuad, RainHalo, RenderInput, Theme};

use crate::cursor_glow::{Geom, GlowStyle};
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
const PAUSE_GLYPH_W: f32 = 14.0;
/// The one action a looping preview's Pause/Play activation invokes.
pub(crate) const PREVIEW_PAUSE_ACTION: &str = "settings/preview-pause";

/// The narrowest host cadence that can change a preview's pixels.
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
    /// The Themes page's demo terminal.
    ThemeDemo,
    /// The Wallpaper page's preview.
    Wallpaper,
}

impl PreviewScene {
    const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance preview",
            Self::Typography => "Typography preview",
            Self::CursorMotion => "Cursor and motion preview",
            Self::ThemeDemo => "Demo terminal",
            Self::Wallpaper => "Wallpaper preview",
        }
    }

    const fn badge_label(self) -> &'static str {
        match self {
            Self::Appearance => "COLOR PREVIEW",
            Self::Typography => "TEXT PREVIEW",
            Self::CursorMotion => "CURSOR PREVIEW",
            Self::ThemeDemo => "DEMO",
            Self::Wallpaper => "WALLPAPER PREVIEW",
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
    /// The demo played three times over with nothing touched on its page.
    Resting,
    /// The viewer paused the looping preview with its Pause/Play control.
    Viewer,
}

impl PreviewPause {
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
            Self::Resting => "nothing on the page has changed for a while".to_string(),
            Self::Viewer => "you paused it".to_string(),
        }
    }

    /// The cause in the paint audit's words ("raw/effective motion … because …").
    pub(crate) const fn audit(self) -> &'static str {
        match self {
            Self::Unspecified => "motion is reduced",
            Self::SeriousMode => "serious mode suppresses decorative motion and post-processing",
            Self::Load => "adaptive performance load-shed is active",
            Self::Unfocused => "the aterm window is unfocused",
            Self::MotionSetting => "motion=reduced explicitly disables motion",
            Self::SystemReduceMotion => "motion=auto follows system Reduce Motion",
            Self::Resting => "the demo rests after three loops with nothing touched on its page",
            Self::Viewer => "the viewer paused the looping preview with its Pause/Play control",
        }
    }

    fn badge(self) -> String {
        match self {
            Self::SeriousMode => "Serious Mode".to_string(),
            Self::Unfocused => "Paused in background".to_string(),
            Self::Load => "Paused under load".to_string(),
            Self::Resting => "Resting".to_string(),
            Self::Viewer => "Paused".to_string(),
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
    /// The classic wake.
    Classic,
}

/// The pet the `cursor_pet` setting keeps beside the cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewTrailCompanion {
    #[default]
    None,
    Pet(aterm_effects::kitty_pet::PetSpecies),
    /// The flier (`cursor_pet = "flying cat"`) beside the rainbow, whose momentum flies it in.
    FlyingKitty,
}

impl PreviewTrailStyle {
    #[cfg(test)]
    pub(crate) fn parse(value: &str) -> Self {
        Self::from_resolution(crate::app_config::resolve_trail_style(value))
    }

    pub(crate) const fn from_resolution(resolved: crate::app_config::ResolvedTrailStyle) -> Self {
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
            None => Self::Off,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lumen => "lumen",
            Self::Phaser => "phaser",
            Self::RainbowKitty => "rainbow",
            Self::Sparkle => "sparkle",
            Self::Fire => "fire",
            Self::Laser => "laser",
            Self::Water => "water",
            Self::Beam => "beam",
            Self::Comet => "comet",
            Self::Classic => "classic",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CursorPreviewSpec {
    pub(crate) style: PreviewCursorStyle,
    pub(crate) blink: bool,
    /// Cursor effects may draw.
    pub(crate) trail_enabled: bool,
    pub(crate) trail_style: PreviewTrailStyle,
    /// The selected preference token.
    pub(crate) trail_style_raw: Arc<str>,
    /// The trail colour setting.
    pub(crate) color: aterm_effects::cursor_fx::TrailColor,
    /// Optional packed `0x00RRGGBB`.
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
    /// The typing-momentum glow switch.
    pub(crate) momentum_glow: bool,
    /// The cursor colour was chosen rather than inherited from the theme.
    pub(crate) cursor_color_pinned: bool,
    /// The animal the `cursor_pet` setting names, beside whatever style is selected.
    pub(crate) pet: Option<aterm_effects::kitty_pet::PetSpecies>,
    /// The `cursor_pet` setting names the flier: no walker, and a head the rainbow flies in.
    pub(crate) pet_flies: bool,
}

impl Default for CursorPreviewSpec {
    fn default() -> Self {
        Self {
            style: PreviewCursorStyle::Block,
            blink: true,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::RainbowKitty,
            trail_style_raw: Arc::from(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            color: aterm_effects::cursor_fx::TrailColor::Automatic,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 1.0,
            radius: 0.6,
            ring: true,
            momentum_glow: true,
            cursor_color_pinned: false,
            pet: aterm_effects::cursor_pet::species(crate::prefs::DEFAULT_CURSOR_PET),
            pet_flies: aterm_effects::cursor_pet::flies(crate::prefs::DEFAULT_CURSOR_PET),
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
            color: self.color.masked(),
            accent: self.accent.map(|color| color & 0x00ff_ffff),
            duration_ms: self.duration_ms.clamp(MIN_DURATION_MS, MAX_DURATION_MS),
            length: self.length.clamp(1, MAX_LENGTH),
            intensity: finite_or(self.intensity, 0.0).clamp(0.0, 1.0),
            radius: finite_or(self.radius, 0.0).clamp(0.0, 2.0),
            ring: self.ring,
            momentum_glow: self.momentum_glow,
            cursor_color_pinned: self.cursor_color_pinned,
            pet: self.pet,
            pet_flies: self.pet_flies,
        }
    }

    /// The selected token when it still names this broad preview style.
    pub(crate) fn trail_style_label(&self) -> String {
        let resolved = self.resolved_trail_style();
        crate::native_settings::setting_choice_label(
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            resolved
                .canonical
                .unwrap_or_else(|| self.effective_trail_style_raw()),
        )
    }

    /// The style spelling aterm.toml names where aterm does not know it.
    pub(crate) fn unknown_trail_spelling(&self) -> Option<&str> {
        let raw = self.effective_trail_style_raw();
        (self.resolved_trail_style().unknown && !raw.is_empty()).then_some(raw)
    }

    /// Authored, not resolved.
    pub(crate) fn effective_trail_style_raw(&self) -> &str {
        let raw = self.trail_style_raw.trim();
        let resolved = crate::app_config::resolve_trail_style(raw);
        if PreviewTrailStyle::from_resolution(resolved) == self.trail_style {
            raw
        } else {
            self.trail_style.label()
        }
    }

    #[cfg(test)]
    fn effective_trail_style_token(&self) -> &str {
        crate::app_config::effective_trail_style_token(self.effective_trail_style_raw())
    }

    fn resolved_trail_style(&self) -> crate::app_config::ResolvedTrailStyle {
        crate::app_config::resolve_trail_style(self.effective_trail_style_raw())
    }

    fn resolved_trail_presentation(&self) -> crate::app_config::ResolvedTrailPresentation {
        let raw = self.effective_trail_style_raw();
        let mut presentation = crate::app_config::resolve_trail_presentation_from_style(
            raw,
            self.resolved_trail_style(),
            "off",
        );
        presentation.pet_species = self.pet;
        presentation.pet_flies = self.pet_flies;
        presentation
    }

    /// The pet the runtime draws: a walker beside every style and the plain cursor, and the
    /// flier beside the rainbow, the one trail whose momentum flies it in.
    pub(crate) fn trail_companion(&self) -> PreviewTrailCompanion {
        match self.pet {
            Some(species) => PreviewTrailCompanion::Pet(species),
            None if self.pet_flies
                && self.resolved_trail_presentation().style.style
                    == Some(GlowStyle::RainbowKitty) =>
            {
                PreviewTrailCompanion::FlyingKitty
            }
            None => PreviewTrailCompanion::None,
        }
    }

    /// The preview's glow config on `theme`, resolved as a window's is, over a card whose page
    /// takes `glass` ([`aterm_effects::host::PageFacts::glass`] for a window's). Its light
    /// (`docs/design/PRISM-GLASS.md` §15.2 1.11) has no latch: the preview's theme is both its
    /// configured scheme and its page, and no program moves either, so the light is
    /// [`crate::cursor_glow::Light::admitted`] the two tests a window's latch gates on
    /// (`scheme_light`, `raw_light`) at `prism_glass::LIVE`, from the first frame, on its page as
    /// a window's is (§15.2 1.7, [`crate::cursor_glow::Light::on_page`]): on a page that takes no
    /// glass the light is sheer and draws main's paper arm. The Cursor card's page keeps a
    /// window's page law (§15.8 amendment 54, [`CursorScene`]'s frame): it takes the glass where
    /// the card's face, at the card's device size, lays its letters full.
    fn glow_config(
        &self,
        theme: Theme,
        head_dx: f32,
        clear: bool,
    ) -> crate::cursor_glow::GlowConfig {
        self.glow_config_at(
            theme,
            head_dx,
            clear,
            aterm_effects::rainbow_kitty::prism_glass::LIVE,
        )
    }

    /// [`Self::glow_config`] at the live level `live`, which the tests preview before `LIVE`
    /// takes it.
    fn glow_config_at(
        &self,
        theme: Theme,
        head_dx: f32,
        clear: bool,
        live: aterm_effects::cursor_glow::Live,
    ) -> crate::cursor_glow::GlowConfig {
        use aterm_effects::host::light_latch::{raw_light, scheme_light};
        let presentation = self.resolved_trail_presentation();
        let (fg, bg, dark_theme) = (
            theme.fg & 0x00FF_FFFF,
            theme.bg & 0x00FF_FFFF,
            theme_bg_is_dark(theme.bg),
        );
        let mut cfg = crate::app_config::resolve_cursor_glow(
            crate::app_config::CursorGlowInputs {
                enabled: self.trail_enabled,
                color: self.color.knob(theme.cursor),
                accent: self.accent,
                duration_ms: self.duration_ms,
                length: self.length,
                intensity: self.intensity,
                radius: self.radius,
                ring: self.ring,
            },
            presentation,
            theme.cursor,
            dark_theme,
            fg,
            bg,
            head_dx,
        );
        cfg.light = crate::cursor_glow::Light::admitted(
            scheme_light(bg) && raw_light(fg, bg, dark_theme),
            live,
        )
        .on_page(clear);
        cfg
    }

    const fn has_resolved_trail(&self) -> bool {
        !matches!(self.trail_style, PreviewTrailStyle::Off)
    }
}

/// A bounded terminal-canvas palette for previewing an uncommitted theme, packed `0x00RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PreviewTerminalTheme {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) cursor: u32,
    pub(crate) selection: u32,
    pub(crate) ansi: [u32; 16],
}

impl PreviewTerminalTheme {
    /// Every slot is a real palette.
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

    /// The colours a resolution draws.
    pub(crate) fn from_resolved(resolved: &crate::resolved_colors::ResolvedColors) -> Self {
        Self::from_scheme(&resolved.scheme)
    }

    fn over_engine_palette(host: Theme) -> Self {
        let ansi = std::array::from_fn(|index| {
            pack_rgb(aterm_types::ColorPalette::default_color(index as u8))
        });
        Self::new(host.fg, host.bg, host.cursor, host.selection, ansi)
    }

    const fn normalized(self) -> Self {
        Self::new(self.fg, self.bg, self.cursor, self.selection, self.ansi)
    }

    pub(crate) const fn into_theme(self) -> Theme {
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

/// Who the candidate palette is, for the words a screen reader speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewThemeIdentity {
    pub(crate) label: String,
    pub(crate) light: bool,
}

impl PreviewThemeIdentity {
    /// `resolved`'s label (a file's `name =`, a built-in's name) and the appearance it reports.
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

const LEGIBLE_ANSI_CONTRAST: f64 = 3.0;

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
        theme_card::contrast_figure(text),
        if illegible == 1 { "colour" } else { "colours" },
    )
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppearancePreviewSpec {
    pub(crate) window_theme: String,
    pub(crate) minimum_contrast: f32,
    /// The engine's theme-pair contrast floor (`TerminalConfig::indexed_minimum_contrast`).
    pub(crate) indexed_minimum_contrast: f32,
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
            indexed_minimum_contrast: crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST,
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
    /// Immutable, already-decoded asset from the same config snapshot as the view.
    pub(crate) kitty_asset: crate::app_config::KittySpriteAsset,
    pub(crate) bloom: bool,
    pub(crate) bloom_strength: f32,
    pub(crate) bloom_radius: f32,
    pub(crate) fire_shimmer: bool,
    pub(crate) hdr_glow: bool,
    pub(crate) sdr_boost: f32,
    /// The window's screen shows edr.
    pub(crate) display_edr: bool,
    pub(crate) motion_raw: String,
    pub(crate) motion_effective: String,
    pub(crate) motion_reason: String,
    pub(crate) adaptive_motion: bool,
    pub(crate) performance_reduced: bool,
    /// The cat the window's companion wears now.
    pub(crate) companion_look: aterm_effects::kitty_registry::KittyLook,
}

/// Whether the preview shows HDR glow dimmer than the terminal.
pub(crate) const fn hdr_glow_capped(hdr_glow: bool, display_edr: bool) -> bool {
    hdr_glow && display_edr
}

/// What the preview cannot show while [`hdr_glow_capped`].
pub(crate) const HDR_PREVIEW_CAVEAT: &str = "The preview can\u{2019}t show HDR brightness; the terminal draws this glow brighter on this display";
/// [`HDR_PREVIEW_CAVEAT`] as the row's tag.
pub(crate) const HDR_PREVIEW_CAVEAT_TAG: &str = "Preview can\u{2019}t show HDR";
/// [`HDR_PREVIEW_CAVEAT`] as the row speaks it.
pub(crate) const HDR_PREVIEW_CAVEAT_SPOKEN: &str = "The preview can\u{2019}t show HDR brightness";
/// What the cursor preview says while Heat shimmer is focused: the shimmer is the GPU's
/// present-time refraction over the flames, and the preview is drawn on the CPU.
pub(crate) const SHIMMER_PREVIEW_CAVEAT: &str = "Preview can\u{2019}t show shimmer";

impl CursorPostFxSpec {
    /// Whether the preview draws HDR glow capped ([`hdr_glow_capped`]).
    pub(crate) const fn hdr_glow_capped(&self) -> bool {
        hdr_glow_capped(self.hdr_glow, self.display_edr)
    }

    fn glow_lift_words(&self) -> String {
        if self.hdr_glow_capped() {
            "HDR glow on an HDR display, shown capped at standard range".to_string()
        } else {
            format!(
                "HDR glow {}{}; SDR crown at SDR glow boost {}",
                if self.hdr_glow { "on" } else { "off" },
                if self.display_edr {
                    ""
                } else {
                    " (no HDR display)"
                },
                trim_float(self.sdr_boost)
            )
        }
    }
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
            display_edr: false,
            motion_raw: "auto".to_string(),
            motion_effective: "full".to_string(),
            motion_reason: "system permits motion".to_string(),
            adaptive_motion: true,
            performance_reduced: false,
            companion_look: aterm_effects::kitty_registry::KittyLook::default(),
        }
    }
}

/// What the Themes demo plays ([`PreviewScene::ThemeDemo`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ThemeDemoSpec {
    /// Milliseconds into the tape's loop.
    pub(crate) phase_ms: u64,
    /// The chrome colours the candidate theme authors (config over theme).
    pub(crate) chrome: aterm_messages::tokens::AuthoredChrome,
    /// The type a typography package sets, in words ("Menlo 1.3×"), for the spoken value.
    pub(crate) sets_type: Option<String>,
    /// The most rows the demo shows.
    pub(crate) max_rows: usize,
}

/// The most rows and columns the demo terminal is laid out at.
pub(crate) const DEMO_MAX_ROWS: usize = 40;
pub(crate) const DEMO_MAX_COLS: usize = 100;
/// The demo window's tab strip height at the 1x text size.
const DEMO_STRIP_HEIGHT: f32 = 28.0;

/// The demo window's tab strip height: [`DEMO_STRIP_HEIGHT`], or where a large text size sets
/// the tab titles bigger, the tabs' 6pt inset over a title line like a header's (the caption
/// with 4pt over and under it).
pub(crate) fn demo_strip_height() -> f32 {
    DEMO_STRIP_HEIGHT.max((header_caption().get() + 14.0).ceil())
}
/// The inset between the demo window's edge and its grid.
pub(crate) const DEMO_GRID_INSET: f32 = 8.0;

/// The window the Themes demo frames its terminal in: the one aterm's own window looks like on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemoFrame {
    /// A Windows 11 aterm window, whose tab row is its title bar.
    Windows,
    /// Three window dots at the left and a two-tab strip whose selected card carries the accent.
    Dots,
}

impl DemoFrame {
    /// The frame this build draws.
    pub(crate) const PLATFORM: Self = if crate::tab_bar::SELECTED_TAB_IS_GROUND {
        Self::Windows
    } else {
        Self::Dots
    };

    /// The window's corner radius.
    pub(crate) const fn radius(self) -> f32 {
        match self {
            Self::Windows => 8.0,
            Self::Dots => 10.0,
        }
    }

    /// The two tabs' titles, the selected one first, each a ladder of whole titles longest first.
    pub(crate) const fn titles(self) -> [&'static [&'static str]; 2] {
        match self {
            Self::Windows => [&["PowerShell", "pwsh"], &["Command Prompt", "cmd"]],
            Self::Dots => [&["zsh — aterm", "zsh"], &["targo test", "targo"]],
        }
    }

    /// The faces the two tabs' titles paint in: Windows sets its selected title semibold.
    pub(crate) const fn faces(self) -> [TextFace; 2] {
        match self {
            Self::Windows => [TextFace::UiBold, TextFace::Ui],
            Self::Dots => [TextFace::Ui, TextFace::Ui],
        }
    }

    const fn title_padding(self) -> [f32; 2] {
        match self {
            Self::Windows => [34.0, 20.0],
            Self::Dots => [20.0, 20.0],
        }
    }
}

const DEMO_CAPTION_BUTTON_W: f32 = 40.0;
const DEMO_CAPTION_GLYPH: f32 = 10.0;
const DEMO_NEW_TAB_W: f32 = 32.0;
const DEMO_TAB_RADIUS: f32 = 8.0;

/// Where a demo window drawn in `rect` puts its strip's pieces ([`DemoStripLayout::of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DemoStripLayout {
    /// The strip's height: [`demo_strip_height`], or the whole of a shorter rect.
    pub(crate) strip_h: f32,
    /// The tabs' top edge.
    pub(crate) tab_top: f32,
    /// The selected tab's left edge.
    pub(crate) tabs_x: f32,
    /// The selected tab's width.
    pub(crate) tab_w: f32,
    /// The quiet tab's width.
    pub(crate) quiet_w: f32,
    /// The title each tab says, the selected one first.
    pub(crate) titles: [Option<&'static str>; 2],
    /// The titles' size.
    pub(crate) title_px: f32,
    /// A Windows frame's caption glyph centres, minimize, maximize and close.
    pub(crate) caption: Option<([f32; 3], f32)>,
    /// A Windows frame's `+` centre, while its tabs are drawn.
    pub(crate) new_tab: Option<f32>,
}

impl DemoStripLayout {
    /// The least tab the strip draws.
    pub(crate) const MIN_TAB_W: f32 = 24.0;

    fn max_tab_w() -> f32 {
        168.0 * crate::native_appearance::text_scale().max(1.0)
    }

    fn tabs(frame: DemoFrame, room: f32, px: f32) -> ([f32; 2], [Option<&'static str>; 2]) {
        let half = (room / 2.0).floor().clamp(0.0, Self::max_tab_w());
        let (faces, padding) = (frame.faces(), frame.title_padding());
        let need = |tab: usize, title: &str| {
            (crate::tray_raster::ui_text_width_for(faces[tab], title, px) + padding[tab]).ceil()
        };
        let [selected, quiet] = frame.titles();
        let mut ladder: Vec<(Option<&'static str>, Option<&'static str>)> = Vec::new();
        for (index, &title) in selected.iter().enumerate() {
            let from = if index == 0 { 0 } else { quiet.len() - 1 };
            ladder.extend(
                quiet[from..]
                    .iter()
                    .map(|&other| (Some(title), Some(other))),
            );
        }
        ladder.extend(selected.iter().map(|&title| (Some(title), None)));
        for (s, q) in ladder {
            let selected_need = s.map_or(Self::MIN_TAB_W, |title| need(0, title));
            let quiet_need = q.map_or(Self::MIN_TAB_W, |title| need(1, title));
            if selected_need + quiet_need > room {
                continue;
            }
            let selected_w = half.clamp(
                selected_need,
                (room - quiet_need).min(Self::max_tab_w().max(selected_need)),
            );
            let quiet_w = (room - selected_w).min(Self::max_tab_w().max(quiet_need));
            return ([selected_w, quiet_w], [s, q]);
        }
        ([half, half], [None, None])
    }

    pub(crate) fn of(rect: LogicalRect, frame: DemoFrame) -> Self {
        let top = rect.y;
        let strip_h = demo_strip_height().min(rect.height);
        let title_px = header_caption().get();
        match frame {
            DemoFrame::Dots => {
                let tabs_x = rect.x + 52.0;
                let ([tab_w, quiet_w], titles) =
                    Self::tabs(frame, (rect.right() - 12.0 - tabs_x).max(0.0), title_px);
                Self {
                    strip_h,
                    tab_top: top + 5.0,
                    tabs_x,
                    tab_w,
                    quiet_w,
                    titles,
                    title_px,
                    caption: None,
                    new_tab: None,
                }
            }
            DemoFrame::Windows => {
                let tabs_x = (rect.x + 8.0).round();
                let buttons = DEMO_CAPTION_BUTTON_W * 3.0;
                let caption = (rect.width >= buttons + 16.0).then(|| {
                    let x = (rect.right() - buttons).round();
                    let centre = |index: f32| x + DEMO_CAPTION_BUTTON_W * (index + 0.5);
                    (
                        [centre(0.0), centre(1.0), centre(2.0)],
                        (top + strip_h * 0.5).round(),
                    )
                });
                let tabs_end = caption.map_or(rect.right(), |_| rect.right() - buttons - 8.0);
                let ([tab_w, quiet_w], titles) = Self::tabs(
                    frame,
                    (tabs_end - DEMO_NEW_TAB_W - tabs_x).floor().max(0.0),
                    title_px,
                );
                Self {
                    strip_h,
                    tab_top: top + 6.0,
                    tabs_x,
                    tab_w,
                    quiet_w,
                    titles,
                    title_px,
                    caption,
                    new_tab: (tab_w >= Self::MIN_TAB_W)
                        .then_some(tabs_x + tab_w + quiet_w + DEMO_NEW_TAB_W * 0.5),
                }
            }
        }
    }

    /// The tabs' height, down to the strip's foot.
    pub(crate) fn tab_h(&self, rect: LogicalRect) -> f32 {
        rect.y + self.strip_h - self.tab_top
    }

    /// The optical centre of the tab labels' row.
    pub(crate) fn label_cy(&self, rect: LogicalRect) -> f32 {
        self.tab_top + (self.tab_h(rect) - 2.0) * 0.5
    }
}

/// How a tried cursor style came to the preview, which decides how it is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StyleTrial {
    /// The keyboard is on the row being tried.
    Keyboard,
    /// The pointer is over the row being tried.
    Pointer,
    /// The preview kept the style it was trying while the keyboard is on its own Pause/Play control.
    Kept,
}

impl StyleTrial {
    /// The header's hint for keeping the tried style, where a gesture keeps it.
    pub(crate) const fn save_hint(self) -> Option<&'static str> {
        match self {
            Self::Keyboard => Some("Return to save"),
            Self::Pointer => Some("Click to save"),
            Self::Kept => None,
        }
    }

    /// The spoken value's words for an unsaved, tried style.
    pub(crate) const fn spoken(self) -> &'static str {
        match self {
            Self::Keyboard => "not saved; Return saves, Escape goes back",
            Self::Pointer => "not saved; click saves, Escape goes back",
            Self::Kept => "not saved; Escape goes back",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsPreviewSpec {
    pub(crate) scene: PreviewScene,
    /// Exact candidate terminal type size in logical points, independent of native-app text scale.
    pub(crate) font_px: f32,
    /// Host physical pixels per logical point.
    pub(crate) terminal_scale: f32,
    /// Renderer-native typography geometry and shaping candidates.
    pub(crate) line_height: f32,
    pub(crate) baseline_adjust: i32,
    pub(crate) ligatures: bool,
    pub(crate) merged_ligatures: bool,
    pub(crate) synthetic_styles: bool,
    /// Uncommitted primary and style-family choices.
    pub(crate) font_candidate: SemanticFontCandidate,
    pub(crate) appearance: AppearancePreviewSpec,
    pub(crate) typography: TypographyPreviewSpec,
    pub(crate) post_fx: CursorPostFxSpec,
    /// The field being edited and its normalized uncommitted value.
    pub(crate) focused_key: String,
    pub(crate) focused_value: String,
    /// The focused setting's own name, for the badge and the spoken summary.
    pub(crate) focused_label: String,
    /// The focused theme is a sample only this preview draws.
    pub(crate) theme_sample: bool,
    /// The focused cursor style is being tried.
    pub(crate) style_trial: Option<StyleTrial>,
    /// The focused row's own verdict when it changes nothing now, in the words the row shows.
    pub(crate) focused_effect: Option<String>,
    pub(crate) font_status: String,
    pub(crate) font_ready_epoch: u64,
    pub(crate) prepared_font: crate::tray_raster::PreparedSemanticFont,
    /// Candidate palette for the inner terminal.
    pub(crate) terminal_theme: Option<PreviewTerminalTheme>,
    /// The candidate palette's label and appearance, for the accessible value.
    pub(crate) terminal_theme_identity: Option<PreviewThemeIdentity>,
    pub(crate) cursor: CursorPreviewSpec,
    /// Injected deterministic animation phase.
    pub(crate) phase_ms: u64,
    /// The resolved motion policy.
    pub(crate) reduced_motion: bool,
    /// The viewer paused this looping demonstration.
    pub(crate) paused: bool,
    /// Keyboard focus is on this preview (a Pause/Play control while it loops).
    pub(crate) focused: bool,
    /// The focus came from the keyboard ([`Self::focused`] too).
    pub(crate) focus_visible: bool,
    /// Why `reduced_motion` holds, for the badge and the spoken summary.
    pub(crate) pause: PreviewPause,
    /// The Themes demo's playback.
    pub(crate) demo: ThemeDemoSpec,
    /// The wallpaper the terminal draws under its default backgrounds, as a tab does.
    pub(crate) wallpaper: Option<crate::widget::SpecimenWallpaper>,
    /// The width the host lays this preview out at, which its header wraps to.
    pub(crate) measure_width: Option<f32>,
    /// The candidate's font slots whose names a resolver did not find, each with the name.
    pub(crate) refused_fonts: Vec<(&'static str, String)>,
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
            typography: TypographyPreviewSpec::default(),
            post_fx: CursorPostFxSpec::default(),
            focused_key: "cursor_trail_style".to_string(),
            focused_value: crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.to_string(),
            focused_label: String::new(),
            theme_sample: false,
            style_trial: None,
            focused_effect: None,
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
            paused: false,
            focused: false,
            focus_visible: false,
            pause: PreviewPause::Unspecified,
            demo: ThemeDemoSpec::default(),
            wallpaper: None,
            measure_width: None,
            refused_fonts: Vec::new(),
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

    /// The Themes demo at `phase_ms` into its loop.
    pub(crate) fn theme_demo(font_px: f32, phase_ms: u64) -> Self {
        Self {
            scene: PreviewScene::ThemeDemo,
            font_px,
            cursor: CursorPreviewSpec {
                trail_enabled: false,
                blink: false,
                ..CursorPreviewSpec::default()
            },
            demo: ThemeDemoSpec {
                phase_ms,
                ..ThemeDemoSpec::default()
            },
            ..Self::default()
        }
    }

    /// The chrome colours the candidate theme authors, alone.
    pub(crate) const fn with_authored_chrome(
        mut self,
        chrome: aterm_messages::tokens::AuthoredChrome,
    ) -> Self {
        self.demo.chrome = chrome;
        self
    }

    /// The demo's chrome colours, its type words and its row bound.
    pub(crate) fn with_demo(
        mut self,
        chrome: aterm_messages::tokens::AuthoredChrome,
        sets_type: Option<String>,
        max_rows: usize,
    ) -> Self {
        self.demo.chrome = chrome;
        self.demo.sets_type = sets_type;
        self.demo.max_rows = max_rows;
        self
    }

    pub(crate) fn cursor(cursor: CursorPreviewSpec) -> Self {
        Self {
            cursor,
            ..Self::default()
        }
    }

    pub(crate) const fn with_phase(mut self, phase_ms: u64) -> Self {
        self.phase_ms = phase_ms;
        self
    }

    /// [`Self::with_phase`] with a phase the host derives from the rest of the spec.
    pub(crate) fn map_phase(self, phase: impl FnOnce(&Self) -> u64) -> Self {
        let phase_ms = phase(&self);
        self.with_phase(phase_ms)
    }

    pub(crate) const fn with_reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.reduced_motion = reduced_motion;
        self
    }

    pub(crate) const fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// Why the preview moves or holds still, in the paint audit's words.
    pub(crate) fn motion_reason(&self) -> &str {
        if self.reduced_motion {
            self.pause.audit()
        } else {
            &self.post_fx.motion_reason
        }
    }

    pub(crate) const fn with_pause(mut self, pause: PreviewPause) -> Self {
        self.pause = pause;
        self
    }

    pub(crate) const fn with_terminal_theme(
        mut self,
        terminal_theme: PreviewTerminalTheme,
    ) -> Self {
        self.terminal_theme = Some(terminal_theme);
        self
    }

    pub(crate) fn with_terminal_theme_identity(mut self, identity: PreviewThemeIdentity) -> Self {
        self.terminal_theme_identity = Some(identity);
        self
    }

    pub(crate) const fn with_typography(
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
    pub(crate) const fn with_theme_sample(mut self, sample: bool) -> Self {
        self.theme_sample = sample;
        self
    }

    /// The font slots a resolver did not find ([`Self::refused_fonts`]).
    pub(crate) fn with_refused_fonts(mut self, refused: Vec<(&'static str, String)>) -> Self {
        self.refused_fonts = refused;
        self
    }

    fn unresolved_slots(&self) -> Vec<(&'static str, String)> {
        let mut slots = self.refused_fonts.clone();
        for (slot, family) in crate::tray_raster::unresolved_slots(&self.font_status) {
            if slots.iter().all(|(known, _)| *known != slot) {
                slots.push((slot, family));
            }
        }
        slots
    }

    /// The width the host lays this preview out at ([`Self::measure_width`]).
    pub(crate) const fn with_measure_width(mut self, width: f32) -> Self {
        self.measure_width = Some(width);
        self
    }

    /// Mark the focused cursor style as tried, not saved.
    pub(crate) const fn with_style_trial(mut self, trial: Option<StyleTrial>) -> Self {
        self.style_trial = trial;
        self
    }

    fn trying_style(&self) -> Option<StyleTrial> {
        self.style_trial
            .filter(|_| self.focused_key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
    }

    /// The focused row's verdict when it changes nothing now, in the row's own words.
    pub(crate) fn with_focused_effect(mut self, effect: Option<String>) -> Self {
        self.focused_effect = effect;
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
                indexed_minimum_contrast: finite_or(self.appearance.indexed_minimum_contrast, 1.0)
                    .clamp(1.0, 21.0),
                selection_foreground: self
                    .appearance
                    .selection_foreground
                    .map(|color| color & 0x00ff_ffff),
                selection_inactive: self.appearance.selection_inactive,
                bold_is_bright: self.appearance.bold_is_bright,
                faint_opacity: finite_or(self.appearance.faint_opacity, 0.5).clamp(0.0, 1.0),
            },
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
                display_edr: self.post_fx.display_edr,
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
            style_trial: self.style_trial,
            focused_effect: self.focused_effect.clone(),
            font_status: self.font_status.clone(),
            font_ready_epoch: self.font_ready_epoch,
            prepared_font: self.prepared_font.clone(),
            terminal_theme: self.terminal_theme.map(PreviewTerminalTheme::normalized),
            terminal_theme_identity: self.terminal_theme_identity.clone(),
            cursor: self.cursor.normalized(),
            phase_ms: self.phase_ms % self.phase_cycle_ms(),
            reduced_motion: self.reduced_motion,
            paused: self.paused,
            focused: self.focused,
            focus_visible: self.focus_visible,
            pause: self.pause,
            demo: ThemeDemoSpec {
                phase_ms: self.demo.phase_ms % crate::settings_theme_demo::DEMO_LOOP_MS,
                chrome: self.demo.chrome,
                sets_type: self
                    .demo
                    .sets_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|words| !words.is_empty())
                    .map(str::to_string),
                max_rows: self.demo.max_rows,
            },
            wallpaper: self.wallpaper.clone().map(|mut wallpaper| {
                wallpaper.dim = finite_or(wallpaper.dim, 0.0).clamp(0.0, 1.0);
                wallpaper
            }),
            measure_width: self.measure_width.filter(|width| width.is_finite()),
            refused_fonts: self.refused_fonts.clone(),
        }
    }

    /// The cadence the pixels this spec can paint require.
    pub(crate) fn animation(&self) -> PreviewAnimation {
        if self.reduced_motion || self.holds_pause() {
            return PreviewAnimation::None;
        }
        self.animation_without_reduction()
    }

    /// The identity of a looping demonstration, everything that decides its replay but the phase.
    pub(crate) fn loop_identity(&self) -> Option<u64> {
        if self.animation() != PreviewAnimation::Continuous {
            return None;
        }
        let mut probe = self.clone().with_phase(0);
        probe.focused = false;
        probe.focus_visible = false;
        probe.font_ready_epoch = 0;
        Some(probe.paint_fingerprint())
    }

    /// Whether this preview offers its Pause/Play control.
    pub(crate) fn pause_control(&self) -> bool {
        self.control_slot() && !self.reduced_motion
    }

    fn control_slot(&self) -> bool {
        self.scene == PreviewScene::CursorMotion
            && self.animation_without_reduction() == PreviewAnimation::Continuous
    }

    /// The activation a looping preview offers.
    pub(crate) fn pause_action(&self) -> Option<crate::native_ui::ActionId> {
        self.pause_control()
            .then(|| crate::native_ui::ActionId::new(PREVIEW_PAUSE_ACTION))
    }

    /// What activating the preview does, for the accessibility description.
    pub(crate) fn semantic_description(&self) -> Option<String> {
        self.pause_control().then(|| {
            if self.holds_pause() {
                "Activate to play the looping trail demonstration again.".to_string()
            } else {
                "Activate to pause the looping trail demonstration on one still frame.".to_string()
            }
        })
    }

    fn holds_pause(&self) -> bool {
        self.paused && self.pause_control()
    }

    fn animation_without_reduction(&self) -> PreviewAnimation {
        if self.scene == PreviewScene::ThemeDemo {
            return crate::settings_theme_demo::demo_animation(self.demo.phase_ms, true);
        }
        if self.scene != PreviewScene::CursorMotion {
            return PreviewAnimation::None;
        }
        if self.cursor.trail_enabled
            && finite_or(self.cursor.intensity, 0.0) > 0.0
            && (self.cursor.momentum_glow || self.cursor.has_resolved_trail())
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
            || self.holds_pause()
            || !self.cursor.blink
            || (self.phase_ms / CURSOR_BLINK_HALF_MS).is_multiple_of(2)
    }

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
        let unresolved = self.unresolved_slots();
        let state = if !unresolved.is_empty() || status.contains("unresolved") {
            "Not found"
        } else if status.contains("unavailable") || status.contains("fallback active") {
            "Unavailable"
        } else if self.prepared_font.snapshot.pending
            || status.contains("loading")
            || status.contains("queued")
            || status.contains("ready for host preparation")
        {
            return Some(("Loading font".to_string(), false));
        } else {
            return None;
        };
        let candidate = &self.font_candidate;
        let failed = unresolved.first().map(|(slot, _)| *slot).and_then(|slot| {
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
        let value = match self.focused_font_slot() {
            Some(Some(family)) => family_words(family),
            Some(None) => self.automatic_words(),
            None if self.focused_value.is_empty() => "system default".to_string(),
            None => self.focused_value.clone(),
        };
        Some((format!("{state} · {label}: {value}"), true))
    }

    fn millis_until_next_blink_edge(&self) -> u64 {
        let cycle = self.phase_cycle_ms();
        let phase = self.phase_ms % cycle;
        for multiple in 1.. {
            let edge = multiple * CURSOR_BLINK_HALF_MS;
            if edge >= cycle {
                break;
            }
            if edge > phase {
                return edge - phase;
            }
        }
        cycle - phase + CURSOR_BLINK_HALF_MS
    }

    fn phase_cycle_ms(&self) -> u64 {
        if self.scene == PreviewScene::CursorMotion {
            CURSOR_SCENE_CYCLE_MS
        } else {
            PHASE_CYCLE_MS
        }
    }

    pub(crate) fn semantic_label(&self) -> String {
        self.scene.label().to_string()
    }

    pub(crate) fn semantic_value(&self) -> String {
        let spec = self.normalized();
        match spec.scene {
            PreviewScene::Appearance => {
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
                let auto_side = spec.terminal_theme.map(|candidate| {
                    if window_sample_light(&spec, candidate.into_theme()) {
                        "Light"
                    } else {
                        "Dark"
                    }
                });
                let window = match spec.appearance.window_theme.as_str() {
                    "light" => "Light window appearance".to_string(),
                    "dark" => "Dark window appearance".to_string(),
                    _ => {
                        let auto = crate::prefs::window_theme_auto(std::env::consts::OS);
                        let side = auto_side
                            .map(|side| format!(", currently {side}"))
                            .unwrap_or_default();
                        format!(
                            "window appearance that matches {}{side}{}",
                            auto.follows, auto.system
                        )
                    }
                };
                let subject = if spec.theme_sample {
                    "Color theme sample"
                } else {
                    "Color theme"
                };
                format!("{subject}: {colors}, with {window}.")
            }
            PreviewScene::ThemeDemo => {
                let colors = spec.terminal_theme.map_or_else(
                    || "the current terminal colors".to_string(),
                    |theme| spoken_palette(theme, spec.terminal_theme_identity.as_ref()),
                );
                let sets_type = spec
                    .demo
                    .sets_type
                    .as_ref()
                    .map_or_else(String::new, |words| format!(", sets type {words}"));
                let state = if spec.reduced_motion {
                    format!(", paused because {}", spec.pause.because())
                } else {
                    ", animated demo session".to_string()
                };
                format!("{colors}{sets_type}{state}")
            }
            PreviewScene::Wallpaper => {
                let words = spec.wallpaper.as_ref().map_or_else(
                    || "no picture".to_string(),
                    |wallpaper| {
                        format!(
                            "{} by {} pixels, dim {}%, text tint {}",
                            wallpaper.w,
                            wallpaper.h,
                            (wallpaper.dim * 100.0).round(),
                            if wallpaper.tint { "on" } else { "off" }
                        )
                    },
                );
                let name = spec.focused_value.split(" · ").next().unwrap_or("").trim();
                if name.is_empty() {
                    format!("Wallpaper preview: {words}.")
                } else {
                    format!("Wallpaper preview: {name}, {words}.")
                }
            }
            PreviewScene::Typography => {
                let regular_missing = spec
                    .unresolved_slots()
                    .first()
                    .is_some_and(|(slot, _)| *slot == "regular");
                let drawn = spec.font_candidate.drawn_family();
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
                    _ => match drawn.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                        Some(family) => family_words(family),
                        None => "the current font".to_string(),
                    },
                };
                let detail = spec
                    .typography_detail()
                    .map_or_else(String::new, |detail| format!(", {detail}"));
                format!(
                    "Text sample in {family} at {} pixels{detail}; ligatures are {} and synthetic styles are {}.",
                    trim_float(spec.font_px),
                    if spec.ligatures { "on" } else { "off" },
                    if spec.synthetic_styles { "on" } else { "off" },
                )
            }
            PreviewScene::CursorMotion => {
                let trail = if spec.cursor.trail_enabled && spec.cursor.has_resolved_trail() {
                    match spec.cursor.unknown_trail_spelling() {
                        Some(raw) => format!(
                            "{} trail, since aterm.toml's \"{raw}\" is not a style",
                            spec.cursor.trail_style_label()
                        ),
                        None => format!("{} trail", spec.cursor.trail_style_label()),
                    }
                } else if spec.cursor.trail_enabled && spec.cursor.momentum_glow {
                    "no trail, glowing as you type".to_string()
                } else {
                    "no trail".to_string()
                };
                let state = if spec.reduction_suppresses_motion() {
                    format!(", paused because {}", spec.pause.because())
                } else if spec.holds_pause() {
                    format!(", paused because {}", PreviewPause::Viewer.because())
                } else {
                    match spec.animation() {
                        // With no trail the glow is all that moves, and the trail's words
                        // already said so ("glowing as you type"): say typing once.
                        PreviewAnimation::Continuous if !spec.cursor.has_resolved_trail() => {
                            String::new()
                        }
                        PreviewAnimation::Continuous => ", shown while typing".to_string(),
                        PreviewAnimation::BlinkEdge { .. } => ", blinking".to_string(),
                        PreviewAnimation::None => String::new(),
                    }
                };
                let lead = match spec.trying_style() {
                    Some(trial) => format!(
                        "Cursor preview, trying {} ({})",
                        spec.cursor.trail_style_label(),
                        trial.spoken()
                    ),
                    None => "Cursor preview".to_string(),
                };
                let idle = spec.focused_idle().map_or_else(String::new, |effect| {
                    format!(" {} · {effect}.", spec.focused_label)
                });
                let range = if spec.post_fx.hdr_glow_capped()
                    && matches!(
                        spec.focused_key.as_str(),
                        crate::prefs::EDIT_HDR_GLOW | crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST
                    ) {
                    format!(" {HDR_PREVIEW_CAVEAT}.")
                } else {
                    String::new()
                };
                format!(
                    "{lead}: {} cursor, {trail}{state}.{idle}{range}",
                    spec.cursor.style.label()
                )
            }
        }
    }

    /// The display faces drawn over the font (`display_font`), by name (`Pixel`,
    /// `Chunky + Pixel`); `None` with none on.
    fn display_layer_words(&self) -> Option<String> {
        let names: Vec<&str> = self
            .font_candidate
            .display
            .iter()
            .filter_map(|id| {
                aterm_render::DISPLAY_FACES
                    .iter()
                    .find(|face| face.id == *id)
                    .map(|face| face.label)
            })
            .collect();
        (!names.is_empty()).then(|| names.join(" + "))
    }

    fn automatic_words(&self) -> String {
        if self.focused_value.starts_with("Automatic") {
            self.focused_value.clone()
        } else {
            "Automatic".to_string()
        }
    }

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
        (self.unresolved_slots().first().map(|(first, _)| *first) == Some(slot)).then_some(slot)
    }

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

    /// Detailed renderer audit projection for introspection and regression oracles.
    pub(crate) fn audit_value(&self) -> String {
        let spec = self.normalized();
        let motion = match spec.animation() {
            PreviewAnimation::None if spec.reduction_suppresses_motion() => {
                "reduced motion; static"
            }
            PreviewAnimation::None if spec.holds_pause() => "paused by the viewer; one frame",
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
        let trail_scope = if trail_emitted {
            if let PreviewTrailCompanion::Pet(species) = spec.cursor.trail_companion() {
                match species {
                    aterm_effects::kitty_pet::PetSpecies::Cat => {
                        "Shared CursorGlow trail geometry and the cat are live."
                    }
                    aterm_effects::kitty_pet::PetSpecies::Dog => {
                        "Shared CursorGlow trail geometry and the dog are live."
                    }
                }
            } else if spec.cursor.trail_companion() == PreviewTrailCompanion::FlyingKitty {
                "Shared CursorGlow rainbow geometry and the CatBaker flying-cat sprite are live."
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
            "live bloom kernel on the CPU: bloom {} strength {} radius {}; fire shimmer {} (the GPU's present-time refraction pass, not drawn here); {}",
            if spec.post_fx.bloom { "on" } else { "off" },
            trim_float(spec.post_fx.bloom_strength),
            trim_float(spec.post_fx.bloom_radius),
            if spec.post_fx.fire_shimmer {
                "on"
            } else {
                "off"
            },
            spec.post_fx.glow_lift_words(),
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
            "The chosen sprite is shown independently in this specimen; in terminal sessions it flies once Pet is Flying cat beside the Rainbow kitty trail."
        } else {
            "The pet follows the Pet setting."
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
            spec.motion_reason(),
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

    /// The frame a Themes demo shows ([`crate::settings_theme_demo::frame_key`]).
    pub(crate) fn demo_frame(&self) -> Option<(usize, bool, u16)> {
        (self.scene == PreviewScene::ThemeDemo).then(|| {
            crate::settings_theme_demo::frame_key(self.demo.phase_ms, !self.reduced_motion)
        })
    }

    /// Paint-only identity folded into [`crate::native_ui::CompiledUi`]'s retained-raster key.
    pub(crate) fn paint_fingerprint(&self) -> u64 {
        let fingerprint = self.paint_identity_hash();
        debug_assert_eq!(
            fingerprint,
            self.normalized().paint_identity_hash(),
            "paint identity must hash normalized values only"
        );
        fingerprint
    }

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
        finite_or(self.appearance.indexed_minimum_contrast, 1.0)
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
        self.post_fx.display_edr.hash(&mut hash);
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
        self.style_trial.hash(&mut hash);
        self.focused_effect.hash(&mut hash);
        self.refused_fonts.hash(&mut hash);
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
        self.cursor.color.masked().hash(&mut hash);
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
        self.cursor.pet.hash(&mut hash);
        self.cursor.pet_flies.hash(&mut hash);
        self.reduced_motion.hash(&mut hash);
        self.pause.hash(&mut hash);
        self.holds_pause().hash(&mut hash);
        (self.focus_visible && self.pause_control()).hash(&mut hash);
        self.wallpaper
            .as_ref()
            .map(|wallpaper| {
                let (fp, w, h, _, tint) = wallpaper.identity();
                (
                    fp,
                    w,
                    h,
                    finite_or(wallpaper.dim, 0.0).clamp(0.0, 1.0).to_bits(),
                    tint,
                )
            })
            .hash(&mut hash);
        let phase_ms = self.phase_ms % self.phase_cycle_ms();
        let cursor_visible = self.reduced_motion
            || !self.cursor.blink
            || (phase_ms / CURSOR_BLINK_HALF_MS).is_multiple_of(2);
        match self.animation() {
            PreviewAnimation::Continuous => {
                (phase_ms / MOTION_STEP_MS).hash(&mut hash);
                cursor_visible.hash(&mut hash);
            }
            PreviewAnimation::BlinkEdge { .. } => cursor_visible.hash(&mut hash),
            PreviewAnimation::None => 0_u64.hash(&mut hash),
        }
        if let Some(frame) = self.demo_frame() {
            frame.hash(&mut hash);
            self.demo.chrome.hash(&mut hash);
            self.demo.max_rows.hash(&mut hash);
        } else if WINDOW_SAMPLE_WEARS_THE_BAND && self.scene == PreviewScene::Appearance {
            self.demo.chrome.hash(&mut hash);
        }
        hash.finish() | 1
    }

    fn header_height(&self) -> f32 {
        self.header_layout(self.measure_width.unwrap_or(f32::INFINITY))
            .height
    }

    fn header_rungs(&self) -> Vec<String> {
        let candidate = self.candidate_badge();
        if candidate.trim().is_empty() {
            return vec![self.live_state()];
        }
        let full = self.with_state_badge(candidate.clone());
        let mut rungs = Vec::with_capacity(4);
        if let Some(hint) = self.trying_style().and_then(StyleTrial::save_hint) {
            rungs.push(format!("{full} · {hint}"));
        }
        rungs.push(full);
        rungs.push(candidate);
        if !self.candidate_is_unsaved()
            && let Some((_, value)) = self.candidate_parts()
        {
            rungs.push(value);
        }
        rungs.dedup();
        rungs
    }

    /// The header, laid out FOR `width`.
    pub(crate) fn header_layout(&self, width: f32) -> PreviewHeader {
        let px = header_caption().get();
        let measure =
            |text: &str| crate::tray_raster::ui_text_width_for(TextFace::UiBold, text, px);
        let control_reserve = if self.control_slot() {
            PAUSE_GLYPH_W + 6.0
        } else {
            0.0
        };
        let available = (width - PREVIEW_INSET * 2.0 - control_reserve).max(0.0);
        let fits = |text: &str| measure(text) <= available + crate::native_ui::TEXT_FIT_SLACK;
        let rungs = self.header_rungs();
        let scene_width = measure(self.scene.badge_label());
        let (scene, lines, says) = match rungs.iter().find(|rung| fits(rung)) {
            Some(rung) => (
                scene_width + 8.0 + measure(rung) <= available + crate::native_ui::TEXT_FIT_SLACK,
                vec![rung.clone()],
                rung.clone(),
            ),
            None => {
                let least = rungs.last().cloned().unwrap_or_default();
                (
                    false,
                    crate::native_settings::wrap_to_box(&least, available, &measure),
                    least,
                )
            }
        };
        let height = header_height_for(px, lines.len());
        PreviewHeader {
            scene,
            lines,
            says,
            available,
            px,
            height,
        }
    }

    /// [`Self::header_layout`] in a card `rect`: the whole statement, or where the card is too
    /// short for its lines, the band without words. A header never cuts a line ("Comet ·…").
    pub(crate) fn header_in(&self, rect: LogicalRect) -> PreviewHeader {
        let header = self.header_layout(rect.width);
        let holds = |height: f32, under: f32| height + under <= rect.height + 0.01;
        let body = self.least_body();
        // The specimen keeps its least row before the header keeps its words; a card too
        // short for that row anyway gives the header its whole height.
        let keeps = if holds(HEADER_BAND, body) {
            holds(header.height, body)
        } else {
            holds(header.height, 4.0)
        };
        if keeps {
            header
        } else {
            header.without_words()
        }
    }

    fn least_body(&self) -> f32 {
        let row = self.typography_row_height();
        5.0 + PREVIEW_INSET
            + match self.scene {
                PreviewScene::CursorMotion | PreviewScene::Wallpaper => row,
                PreviewScene::Typography => row + 4.0,
                PreviewScene::Appearance => appearance_terminal_floor(row),
                PreviewScene::ThemeDemo => 0.0,
            }
    }

    /// The words this preview paints itself, drawn in `rect`, for the fit audit.
    pub(crate) fn text_runs(&self, rect: LogicalRect) -> Vec<PreviewTextRun> {
        let spec = self.normalized();
        if rect.width <= 2.0 || rect.height <= 2.0 {
            return Vec::new();
        }
        if spec.scene == PreviewScene::ThemeDemo {
            let frame = DemoFrame::PLATFORM;
            let layout = DemoStripLayout::of(rect, frame);
            if layout.tab_w < DemoStripLayout::MIN_TAB_W {
                return Vec::new();
            }
            let (faces, padding) = (frame.faces(), frame.title_padding());
            return layout
                .titles
                .iter()
                .zip([layout.tab_w, layout.quiet_w])
                .enumerate()
                .filter_map(|(tab, (title, width))| {
                    title.map(|title| PreviewTextRun {
                        says: title.to_string(),
                        painted: title.to_string(),
                        required: crate::tray_raster::ui_text_width_for(
                            faces[tab],
                            title,
                            layout.title_px,
                        ),
                        available: width - padding[tab],
                    })
                })
                .collect();
        }
        let header = spec.header_in(rect);
        let width =
            |text: &str| crate::tray_raster::ui_text_width_for(TextFace::UiBold, text, header.px);
        let widest = header
            .lines
            .iter()
            .map(|line| width(line))
            .fold(0.0_f32, f32::max);
        let mut runs = Vec::new();
        if !header.lines.is_empty() {
            runs.push(PreviewTextRun {
                painted: header.says.clone(),
                says: header.says.clone(),
                required: if header.scene {
                    width(spec.scene.badge_label()) + 8.0 + widest
                } else {
                    widest
                },
                available: header.available,
            });
        }
        let terminal = spec.terminal_rect(rect);
        if spec.scene == PreviewScene::Typography
            && spec.shows_font_pill(terminal)
            && let Some(pill) =
                spec.pill_layout(terminal.width, crate::native_appearance::text_scale())
            && terminal.height >= pill.height + 8.0
        {
            runs.push(PreviewTextRun {
                required: pill
                    .lines
                    .iter()
                    .map(|line| {
                        crate::tray_raster::ui_text_width_for(TextFace::UiBold, line, pill.px)
                    })
                    .fold(0.0_f32, f32::max),
                says: pill.says.clone(),
                painted: pill.says,
                available: pill.budget,
            });
        }
        runs
    }

    fn pill_layout(&self, terminal_width: f32, text_scale: f32) -> Option<PillLayout> {
        let (label, warning) = self.font_candidate_pill()?;
        let px = TypeStep::Caption.px(11.5).scaled(text_scale).get();
        let budget = (terminal_width - 26.0).max(0.0);
        let measure =
            |text: &str| crate::tray_raster::ui_text_width_for(TextFace::UiBold, text, px);
        let mut rungs = vec![label.clone()];
        if let Some((head, _)) = label.split_once(": ") {
            rungs.push(head.to_string());
        }
        if let Some((head, _)) = label.split_once(" · ") {
            rungs.push(head.to_string());
        }
        rungs.dedup();
        let (lines, says) = match rungs
            .iter()
            .find(|rung| measure(rung) <= budget + crate::native_ui::TEXT_FIT_SLACK)
        {
            Some(rung) => (vec![rung.clone()], rung.clone()),
            None => {
                let least = rungs.last().cloned().unwrap_or_default();
                (
                    crate::native_settings::wrap_to_box(&least, budget, &measure),
                    least,
                )
            }
        };
        let height = font_candidate_pill_height(text_scale)
            + lines.len().saturating_sub(1) as f32 * (px * 1.3).ceil();
        Some(PillLayout {
            lines,
            says,
            warning,
            px,
            budget,
            height,
        })
    }

    fn shows_font_pill(&self, terminal: LogicalRect) -> bool {
        let Some(pill) = self.pill_layout(terminal.width, crate::native_appearance::text_scale())
        else {
            return false;
        };
        let pill_band = pill.height + 10.0;
        let row = self.typography_row_height();
        let with_pill = ((terminal.height - pill_band).max(0.0) / row).floor() as usize;
        with_pill > 0 || (pill.warning && terminal.height >= pill_band)
    }

    /// Typography keeps the candidate's exact size.
    pub(crate) fn preferred_height(&self, minimum: f32) -> f32 {
        if self.scene != PreviewScene::Typography {
            return minimum;
        }
        minimum.max(self.typography_height(TYPOGRAPHY_KEPT_ROWS))
    }

    /// The least height this preview still demonstrates at.
    pub(crate) fn minimum_height(&self, floor: f32) -> f32 {
        self.least_height(floor, &|spec: &Self| spec.header_height())
    }

    /// [`Self::minimum_height`] with a header of one line, whatever it says: the least height a
    /// host that holds the preview still while its header changes (a style tried under the
    /// pointer, "Trying: …") keeps, where the specimen always shows under the header's band.
    pub(crate) fn steady_minimum_height(&self, floor: f32) -> f32 {
        let one_line = header_height_for(header_caption().get(), 1);
        self.least_height(floor, &|_: &Self| one_line)
    }

    fn least_height(&self, floor: f32, header_of: &dyn Fn(&Self) -> f32) -> f32 {
        let mut spec = self.normalized();
        floor.max(match self.scene {
            PreviewScene::Typography => self.typography_height(1),
            PreviewScene::CursorMotion => {
                let row = spec.typography_row_height();
                let header = header_of(&spec);
                spec.reduced_motion = !spec.reduced_motion;
                (header.max(header_of(&spec)) + 15.0 + row)
                    .max(crate::native_settings::scaled_control_height())
            }
            PreviewScene::Appearance => {
                header_of(&spec) + 15.0 + appearance_terminal_floor(spec.typography_row_height())
            }
            PreviewScene::ThemeDemo => spec.demo_height(6),
            PreviewScene::Wallpaper => header_of(&spec) + 15.0 + 2.0 * spec.typography_row_height(),
        })
    }

    /// The demo terminal's cell in logical points.
    pub(crate) fn demo_cell_size(&self) -> (f32, f32) {
        let scale = finite_or(self.terminal_scale, 1.0).max(f32::EPSILON);
        self.prepared_font
            .specimen_cell_size(
                self.font_px * scale,
                self.line_height,
                &self.variation_requests(),
            )
            .filter(|&(w, h)| w > 0 && h > 0)
            .map_or(
                (self.font_px * 0.6, self.font_px * self.line_height * 1.25),
                |(w, h)| (w as f32 / scale, h as f32 / scale),
            )
    }

    /// The demo window's height with `rows` whole rows.
    pub(crate) fn demo_height(&self, rows: usize) -> f32 {
        let (_, cell_h) = self.demo_cell_size();
        (demo_strip_height() + DEMO_GRID_INSET * 2.0 + rows as f32 * cell_h).ceil()
    }

    /// The demo grid a `rect` holds.
    pub(crate) fn demo_grid(&self, rect: LogicalRect) -> (usize, usize) {
        let (cell_w, cell_h) = self.demo_cell_size();
        let width = (rect.width - DEMO_GRID_INSET * 2.0).max(0.0);
        let height = (rect.height - demo_strip_height() - DEMO_GRID_INSET * 2.0).max(0.0);
        let bound = match self.demo.max_rows {
            0 => DEMO_MAX_ROWS,
            rows => rows.min(DEMO_MAX_ROWS),
        };
        (
            ((height / cell_h).floor() as usize).clamp(1, bound),
            ((width / cell_w).floor() as usize).clamp(8, DEMO_MAX_COLS),
        )
    }

    /// The height at which a color specimen shows every row it has.
    pub(crate) fn whole_color_sample_height(&self) -> Option<f32> {
        self.color_sample_height_at_most(f32::INFINITY)
    }

    /// The tallest color sample no taller than `height` whose rows end on a row boundary.
    pub(crate) fn color_sample_height_at_most(&self, height: f32) -> Option<f32> {
        (self.scene == PreviewScene::Appearance).then(|| {
            let spec = self.normalized();
            let row = spec.typography_row_height();
            let at = |rows: usize| {
                (spec.header_height() + 15.0 + appearance_terminal_floor(rows as f32 * row)).ceil()
            };
            (1..=APPEARANCE_SPECIMEN_ROWS)
                .rev()
                .map(at)
                .find(|&tall| tall <= height + 0.01)
                .unwrap_or_else(|| at(1))
        })
    }

    /// The tallest preview no taller than `height` whose specimen ends on a whole row.
    pub(crate) fn whole_rows_at_most(&self, height: f32) -> f32 {
        let least = self.minimum_height(48.0);
        let spec = self.normalized();
        let row = spec.typography_row_height().max(1.0);
        let fitted = match self.scene {
            PreviewScene::Appearance => self.color_sample_height_at_most(height).unwrap_or(height),
            PreviewScene::CursorMotion => {
                let fixed = spec.header_height() + 15.0;
                let rows = ((height - fixed) / row).floor().max(1.0);
                (fixed + rows * row).ceil()
            }
            PreviewScene::Typography => {
                let fixed = self.typography_height(0);
                let rows = ((height - fixed) / row).floor().max(1.0);
                fixed + rows * row
            }
            PreviewScene::ThemeDemo | PreviewScene::Wallpaper => height,
        };
        fitted.min(height.max(least)).max(least)
    }

    fn typography_height(&self, rows: usize) -> f32 {
        let spec = self.normalized();
        let text_scale = crate::native_appearance::text_scale();
        let terminal_width = spec
            .measure_width
            .map_or(f32::INFINITY, |width| width - PREVIEW_INSET * 2.0);
        let pill = spec.pill_layout(terminal_width, text_scale).map_or_else(
            || font_candidate_pill_height(text_scale),
            |pill| pill.height,
        );
        spec.header_height() + 15.0 + rows as f32 * spec.typography_row_height() + pill + 10.0
    }

    fn variation_requests(&self) -> Vec<(u32, f32)> {
        self.typography
            .variations
            .iter()
            .map(|variation| (variation.tag, variation.value()))
            .collect()
    }

    fn typography_row_height(&self) -> f32 {
        self.row_height_at(self.font_px)
    }

    fn row_height_at(&self, font_px: f32) -> f32 {
        let scale = finite_or(self.terminal_scale, 1.0).max(1.0);
        let variations = self.variation_requests();
        let at = |scale: f32| {
            self.prepared_font
                .specimen_cell_height(font_px * scale, self.line_height, &variations)
                .map(|height| height as f32 / scale)
        };
        match (at(1.0), at(scale)) {
            (Some(one), Some(device)) => one.max(device),
            (Some(one), None) => one,
            _ => font_px * self.line_height * 1.5,
        }
    }

    fn appearance_fit(&self, terminal: LogicalRect) -> (f32, f32) {
        let columns = appearance_row_columns(self);
        if let Some(px) = self.appearance_fit_px(terminal, &columns, 0.0) {
            return (px, 0.0);
        }
        let drop = APPEARANCE_WINDOW_SAMPLE_HEIGHT + 1.0;
        (
            self.appearance_fit_px(terminal, &columns, drop)
                .unwrap_or_else(|| self.appearance_floor_px()),
            drop,
        )
    }

    fn appearance_floor_px(&self) -> f32 {
        (crate::FONT_PX_MIN / self.terminal_scale).min(self.font_px)
    }

    fn appearance_fit_px(
        &self,
        terminal: LogicalRect,
        columns: &[usize],
        drop: f32,
    ) -> Option<f32> {
        let floor = self.appearance_floor_px();
        let mut px = self.font_px;
        for _ in 0..64 {
            let Some(ratio) = self.appearance_overflow(px, terminal, columns, drop) else {
                return Some(px);
            };
            if px <= floor {
                return None;
            }
            px = (px * ratio).min(px - 0.25).max(floor);
        }
        None
    }

    fn appearance_overflow(
        &self,
        px: f32,
        terminal: LogicalRect,
        columns: &[usize],
        drop: f32,
    ) -> Option<f32> {
        let scale = self.terminal_scale;
        let (cell_w, cell_h) = self.prepared_font.specimen_cell_size(
            px * scale,
            self.line_height,
            &self.variation_requests(),
        )?;
        let (cell_w, cell_h) = (cell_w as f32 / scale, cell_h as f32 / scale);
        let rows = appearance_whole_rows(terminal, drop, self.row_height_at(px)).min(columns.len());
        let band = appearance_window_band(terminal);
        let mut worst: Option<f32> = None;
        for (index, &cols) in columns[..rows].iter().enumerate() {
            let top = drop + index as f32 * cell_h;
            let room = match band {
                Some(band) if top < band.bottom => band.text_room,
                _ => terminal.width,
            };
            let ink = cols as f32 * cell_w;
            if ink > room {
                let ratio = room.max(0.0) / ink;
                worst = Some(worst.map_or(ratio, |worst: f32| worst.min(ratio)));
            }
        }
        worst
    }

    fn terminal_specimen(&self, theme: Theme, rows: Option<usize>) -> TerminalSpecimenSpec {
        let (input, input_fingerprint) = build_terminal_specimen_input_rows(self, theme, rows);
        self.specimen_of(theme, input, input_fingerprint, rows)
    }

    fn paint_theme_demo(
        &self,
        prims: &mut Vec<DrawPrim>,
        rect: LogicalRect,
        theme: Theme,
        roles: Roles,
        frame: DemoFrame,
    ) {
        let candidate = self
            .terminal_theme
            .unwrap_or_else(|| PreviewTerminalTheme::over_engine_palette(theme));
        let terminal_theme = candidate.into_theme();
        let strip = crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
            theme: terminal_theme,
            ansi: Some(demo_ansi_hues(&candidate)),
            authored: self.demo.chrome,
        });
        let light_page = !crate::native_appearance::surface_is_dark(roles.surface);
        let radius = frame.radius();
        push_demo_shadow(prims, rect, light_page, true, radius);
        let background = crate::settings::u32_rgb(terminal_theme.bg);
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            radius,
            fill: rgba(background, 255),
        });
        let layout = DemoStripLayout::of(rect, frame);
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: layout.strip_h,
            radius,
            fill: rgba(strip.band, 255),
        });
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y + layout.strip_h * 0.5,
            w: rect.width,
            h: layout.strip_h * 0.5,
            radius: 0.0,
            fill: rgba(strip.band, 255),
        });
        let caption = TypeStep::Caption
            .px(13.75)
            .scaled(crate::native_appearance::text_scale());
        match frame {
            DemoFrame::Dots => paint_demo_dots_strip(prims, rect, &layout, &strip, roles, caption),
            DemoFrame::Windows => paint_demo_windows_strip(
                prims,
                rect,
                &layout,
                &strip,
                background,
                crate::settings::u32_rgb(terminal_theme.fg),
                caption,
            ),
        }
        if let Some(grid) = Self::demo_grid_rect(rect) {
            let (rows, cols) = self.demo_grid(rect);
            let (_, cell_h) = self.demo_cell_size();
            if grid.height >= cell_h && grid.width > 0.0 {
                let colors = crate::settings_theme_demo::DemoColors::new(
                    candidate,
                    self.appearance.selection_foreground,
                    self.appearance.bold_is_bright,
                    self.appearance.indexed_minimum_contrast,
                );
                let (input, fingerprint) = crate::settings_theme_demo::demo_frame(
                    &colors,
                    self.demo.phase_ms,
                    !self.reduced_motion,
                    rows,
                    cols,
                );
                prims.push(DrawPrim::ClipPush {
                    x: grid.x,
                    y: grid.y,
                    w: grid.width,
                    h: grid.height,
                });
                prims.push(DrawPrim::TerminalSpecimen {
                    x: grid.x,
                    y: grid.y,
                    spec: Box::new(self.specimen_of(terminal_theme, input, fingerprint, None)),
                });
                prims.push(DrawPrim::ClipPop);
            }
        }
        let hairline = self.demo.chrome.border.unwrap_or(roles.separator);
        prims.push(DrawPrim::Stroke {
            x: rect.x + 0.5,
            y: rect.y + 0.5,
            w: (rect.width - 1.0).max(0.0),
            h: (rect.height - 1.0).max(0.0),
            radius: radius - 0.5,
            width: 1.0,
            color: rgba(hairline, 255),
        });
    }

    /// [`Self::paint`] of a Themes demo in `frame`, whatever this platform draws.
    #[cfg(test)]
    pub(crate) fn paint_demo_frame(
        &self,
        prims: &mut Vec<DrawPrim>,
        rect: LogicalRect,
        theme: Theme,
        roles: Roles,
        frame: DemoFrame,
    ) {
        self.normalized()
            .paint_theme_demo(prims, rect, theme, roles, frame);
    }

    fn demo_grid_rect(rect: LogicalRect) -> Option<LogicalRect> {
        let full = demo_strip_height();
        let strip_h = full.min(rect.height);
        (strip_h >= full).then(|| {
            LogicalRect::new(
                rect.x + DEMO_GRID_INSET,
                rect.y + strip_h + DEMO_GRID_INSET,
                (rect.width - DEMO_GRID_INSET * 2.0).max(0.0),
                (rect.height - strip_h - DEMO_GRID_INSET * 2.0).max(0.0),
            )
        })
    }

    /// The terminal body of a preview drawn in `rect`.
    pub(crate) fn terminal_rect(&self, rect: LogicalRect) -> LogicalRect {
        let header_h = self.normalized().header_in(rect).height;
        LogicalRect::new(
            rect.x + PREVIEW_INSET,
            rect.y + header_h + 5.0,
            (rect.width - PREVIEW_INSET * 2.0).max(1.0),
            (rect.height - header_h - PREVIEW_INSET - 5.0).max(1.0),
        )
    }

    /// What one tick of this preview's motion can repaint, of a node drawn in `rect`.
    pub(crate) fn tick_rect(&self, rect: LogicalRect) -> LogicalRect {
        if rect.width <= 2.0 || rect.height <= 2.0 {
            return rect;
        }
        match self.scene {
            PreviewScene::ThemeDemo => Self::demo_grid_rect(rect).unwrap_or(rect),
            PreviewScene::CursorMotion => {
                let terminal = self.terminal_rect(rect);
                let spec = self.normalized();
                let scale = finite_or(spec.terminal_scale, 1.0).max(f32::EPSILON);
                let frame = spec
                    .prepared_font
                    .specimen_cell_size(
                        spec.font_px * scale,
                        spec.line_height,
                        &spec.variation_requests(),
                    )
                    .map(|(cell_w, _)| (CURSOR_SCENE_COLS * cell_w) as f32 / scale);
                match frame {
                    Some(width) => LogicalRect::new(
                        terminal.x,
                        terminal.y,
                        (width + 1.0).min(terminal.width),
                        terminal.height,
                    ),
                    None => terminal,
                }
            }
            _ => self.terminal_rect(rect),
        }
    }

    /// What changing this spec to `next` repaints of a node drawn in `rect`.
    pub(crate) fn repaint_rect(&self, next: &Self, rect: LogicalRect) -> LogicalRect {
        // Two specs that paint the same pixels repaint nothing: a still
        // preview whose phase moved on (every spec carries the page's clock)
        // is not re-rastered on another preview's tick.
        if self.paint_fingerprint() == next.paint_fingerprint() {
            return LogicalRect::new(rect.x, rect.y, 0.0, 0.0);
        }
        if self.scene != PreviewScene::ThemeDemo || next.scene != PreviewScene::ThemeDemo {
            let mut moved = self.clone();
            moved.phase_ms = next.phase_ms;
            return if moved == *next {
                self.tick_rect(rect)
            } else {
                rect
            };
        }
        let mut moved = self.clone();
        moved.demo.phase_ms = next.demo.phase_ms;
        moved.phase_ms = next.phase_ms;
        moved.reduced_motion = next.reduced_motion;
        moved.pause = next.pause;
        moved.font_candidate = next.font_candidate.clone();
        moved.font_ready_epoch = next.font_ready_epoch;
        moved.prepared_font = next.prepared_font.clone();
        if moved == *next {
            self.tick_rect(rect)
        } else {
            rect
        }
    }

    fn specimen_of(
        &self,
        theme: Theme,
        input: Arc<aterm_render::RenderInput>,
        input_fingerprint: u64,
        card_rows: Option<usize>,
    ) -> TerminalSpecimenSpec {
        TerminalSpecimenSpec {
            input,
            input_fingerprint,
            motion: (self.scene == PreviewScene::CursorMotion)
                .then(|| Arc::new(CursorScene::new(self, theme, card_rows))),
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
            wallpaper: self.wallpaper.clone(),
        }
    }

    /// The side the window sample paints (`true` = Light) against `host`.
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
        if spec.scene == PreviewScene::ThemeDemo {
            spec.paint_theme_demo(prims, rect, theme, roles, DemoFrame::PLATFORM);
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

        let inset = PREVIEW_INSET;
        let header = spec.header_in(rect);
        let caption_size = header_caption();
        let first_line = header.first_line();
        let caption_y =
            crate::tray_raster::row_baseline(rect.y + 4.0, first_line, caption_size.get());
        let control_reserve = if spec.control_slot() {
            PAUSE_GLYPH_W + 6.0
        } else {
            0.0
        };
        if spec.pause_control() {
            paint_pause_glyph(
                prims,
                rect.right() - inset - PAUSE_GLYPH_W,
                rect.y + 4.0,
                first_line,
                spec.holds_pause(),
                roles.text_secondary,
            );
            if spec.focus_visible {
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 1.0,
                    y: rect.y + 1.0,
                    w: (rect.width - 2.0).max(0.0),
                    h: (rect.height - 2.0).max(0.0),
                    radius: 10.0,
                    width: 2.0,
                    color: rgba(roles.accent, 255),
                });
            }
        }
        if header.scene {
            prims.push(text_prim(
                rect.x + inset,
                caption_y,
                spec.scene.badge_label().to_string(),
                caption_size,
                TextWeight::Regular,
                TextFace::UiBold,
                rgba(roles.accent_ink, 255),
            ));
        }
        let state_ink = rgba(
            if spec.reads_live() {
                roles.success
            } else {
                roles.text_secondary
            },
            255,
        );
        let pitch = header_line_pitch(header.px);
        let single = header.lines.len() == 1;
        for (index, line) in header.lines.iter().enumerate() {
            let line_width =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, line, header.px);
            let x = if single {
                header_state_x(rect, inset, control_reserve, line_width)
            } else {
                rect.x + inset
            };
            prims.push(text_prim(
                x,
                caption_y + index as f32 * pitch,
                line.clone(),
                caption_size,
                TextWeight::Regular,
                TextFace::UiBold,
                state_ink,
            ));
        }

        let terminal = spec.terminal_rect(rect);
        let terminal_theme = spec
            .terminal_theme
            .map_or(theme, PreviewTerminalTheme::into_theme);
        let background = crate::settings::u32_rgb(terminal_theme.bg);
        let (spec, drop) = if spec.scene == PreviewScene::Appearance {
            let (font_px, drop) = spec.appearance_fit(terminal);
            (Self { font_px, ..spec }, drop)
        } else {
            (spec, 0.0)
        };

        let row = spec.typography_row_height();
        let pill_band = spec
            .pill_layout(terminal.width, crate::native_appearance::text_scale())
            .map_or(0.0, |pill| pill.height)
            + 10.0;
        let mut shows_pill = false;
        let whole_rows = match spec.scene {
            PreviewScene::Appearance | PreviewScene::ThemeDemo => {
                appearance_whole_rows(terminal, drop, row)
            }
            PreviewScene::Wallpaper => {
                ((terminal.height.max(0.0) / row).floor() as usize).min(APPEARANCE_SPECIMEN_ROWS)
            }
            PreviewScene::Typography => {
                let with_pill = ((terminal.height - pill_band).max(0.0) / row).floor() as usize;
                let bare = ((terminal.height - 4.0).max(0.0) / row).floor() as usize;
                shows_pill = spec.shows_font_pill(terminal);
                if shows_pill { with_pill } else { bare }.min(TYPOGRAPHY_SPECIMEN_ROWS)
            }
            PreviewScene::CursorMotion => {
                ((terminal.height.max(0.0) / row).floor() as usize).min(CURSOR_SPECIMEN_ROWS)
            }
        };
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
            let terminal_specimen = if spec.wallpaper.is_some() {
                let (cell_w, cell_h) = spec.demo_cell_size();
                let grid = (
                    ((terminal.height - drop) / cell_h.max(1.0)).ceil().max(1.0) as usize,
                    (terminal.width / cell_w.max(1.0)).ceil().max(1.0) as usize,
                );
                let (input, fingerprint) = build_terminal_specimen_input_grid(
                    &spec,
                    terminal_theme,
                    Some(whole_rows),
                    Some(grid),
                );
                spec.specimen_of(terminal_theme, input, fingerprint, Some(whole_rows))
            } else {
                spec.terminal_specimen(terminal_theme, Some(whole_rows))
            };
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
                y: terminal.y + drop,
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

    fn live_state(&self) -> String {
        match self.focused_idle() {
            Some(effect) if !self.reduction_suppresses_motion() => effect,
            _ => self.state_badge(),
        }
    }

    /// The facts the cursor scene draws its trail in, for the knob law (`GlowStyle::reads_knob`):
    /// its ground, cursor shape, ring and classic face. Its jumps are [`scene_jumps`].
    pub(crate) fn scene_knob_context(&self) -> aterm_effects::cursor_glow::KnobContext {
        let dark = self
            .terminal_theme
            .is_none_or(|theme| theme_bg_is_dark(theme.bg));
        aterm_effects::cursor_glow::KnobContext {
            dark,
            bar: matches!(self.cursor.style, PreviewCursorStyle::Bar),
            ring: self.cursor.ring,
            classic_tinted: self.cursor.color.chosen()
                || GlowStyle::style_names_classic_mono(self.cursor.effective_trail_style_raw()),
            over_text: false,
        }
    }

    /// The Trail length from which the scene draws alike at every longer length, the longest
    /// [`GlowStyle::jump_reach`] of its jumps (`None`: Length shapes nothing it draws).
    pub(crate) fn scene_length_reach(&self) -> Option<f32> {
        let glow = self.cursor.resolved_trail_style().style?;
        let ctx = self.scene_knob_context();
        let (cell_w, cell_h) = self.demo_cell_size();
        let aspect = cell_h / cell_w.max(1.0);
        scene_jumps()
            .into_iter()
            .filter_map(|jump| {
                glow.jump_reach(
                    jump.rows,
                    jump.cols,
                    aspect,
                    aterm_effects::cursor_glow::KnobContext {
                        over_text: jump.over_letters,
                        ..ctx
                    },
                )
            })
            .reduce(f32::max)
    }

    fn focused_idle(&self) -> Option<String> {
        if let Some(effect) = &self.focused_effect {
            return Some(effect.clone());
        }
        if self.scene == PreviewScene::CursorMotion
            && self.focused_key == crate::prefs::EDIT_CURSOR_FIRE_SHIMMER
        {
            return Some(SHIMMER_PREVIEW_CAVEAT.to_string());
        }
        if self.scene != PreviewScene::CursorMotion
            || self.focused_key != crate::prefs::EDIT_CURSOR_TRAIL_LENGTH
            || self.animation() != PreviewAnimation::Continuous
            || self.cursor.resolved_trail_style().style.is_none()
        {
            return None;
        }
        match self.scene_length_reach() {
            None => Some("Not shown in this preview".to_string()),
            Some(reach) if self.cursor.length as f32 >= reach => Some(format!(
                "Preview shows up to {}",
                crate::prefs::integer_reading(
                    crate::prefs::EDIT_CURSOR_TRAIL_LENGTH,
                    reach.ceil() as i64
                )
            )),
            Some(_) => None,
        }
    }

    fn reads_live(&self) -> bool {
        matches!(self.animation(), PreviewAnimation::Continuous) && self.focused_idle().is_none()
    }

    fn state_badge(&self) -> String {
        if self.reduction_suppresses_motion() {
            self.pause.badge()
        } else if self.holds_pause() {
            PreviewPause::Viewer.badge()
        } else {
            match self.animation() {
                PreviewAnimation::None => match self.scene {
                    PreviewScene::Appearance => "Color sample".to_string(),
                    PreviewScene::Typography => "Text sample".to_string(),
                    PreviewScene::CursorMotion => "Static preview".to_string(),
                    PreviewScene::ThemeDemo => "Demo".to_string(),
                    PreviewScene::Wallpaper => "Picture sample".to_string(),
                },
                PreviewAnimation::BlinkEdge { .. } => "Live cursor blink".to_string(),
                PreviewAnimation::Continuous if !self.cursor.has_resolved_trail() => {
                    "Live typing glow".to_string()
                }
                PreviewAnimation::Continuous => "Live trail".to_string(),
            }
        }
    }

    fn with_state_badge(&self, candidate: String) -> String {
        let state = self.live_state();
        let redundant = if self.focused_key == crate::prefs::EDIT_MOTION {
            self.reduction_suppresses_motion()
        } else {
            self.focused_key == crate::prefs::EDIT_THEME
                && self.scene == PreviewScene::Appearance
                && self.animation() == PreviewAnimation::None
                && !self.reduction_suppresses_motion()
        };
        if redundant {
            candidate
        } else {
            format!("{candidate} · {state}")
        }
    }

    fn candidate_parts(&self) -> Option<(String, String)> {
        let key = self.focused_key.as_str();
        let subject = match key {
            crate::prefs::EDIT_THEME if self.theme_sample => "Sample",
            crate::prefs::EDIT_THEME => "Theme",
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE if self.style_trial.is_some() => "Trying",
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
            let words = match self.unresolved_focused_slot() {
                Some("regular") => "Current font".to_string(),
                Some("bold" | "italic" | "bold italic") => "Regular face".to_string(),
                Some(_) => self.automatic_words(),
                None => family.map_or_else(|| self.automatic_words(), family_words),
            };
            // A text font's caption names the display layer drawn over it (its bold,
            // italic and bold italic too: the layer draws every style of the characters
            // it covers), so what the specimen draws stays named while the faces card
            // scrolls out of view. Symbols and emoji are not drawn over.
            match self.display_layer_words() {
                Some(display)
                    if matches!(
                        key,
                        crate::prefs::EDIT_FONT_FAMILY
                            | crate::prefs::EDIT_FONT_FAMILY_BOLD
                            | crate::prefs::EDIT_FONT_FAMILY_ITALIC
                            | crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC
                    ) =>
                {
                    format!("{display} over {words}")
                }
                _ => words,
            }
        } else if crate::prefs::edit_kind(key) == crate::prefs::EditKind::Bool {
            if raw.eq_ignore_ascii_case("true") {
                "On".to_string()
            } else {
                "Off".to_string()
            }
        } else if key == crate::prefs::EDIT_THEME
            && let Some(identity) = &self.terminal_theme_identity
        {
            identity.label.clone()
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE && !self.cursor.trail_enabled {
            "Off".to_string()
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE
            && raw.trim() == self.cursor.effective_trail_style_raw()
        {
            crate::native_settings::cursor_style_name(raw)
        } else if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE {
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_MOTION {
            // The words the Motion control shows, not the raw value.
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_WINDOW_THEME {
            let raw = raw.strip_suffix(" (default)").unwrap_or(raw);
            crate::native_settings::setting_choice_label(key, raw)
        } else if key == crate::prefs::EDIT_FONT_PX
            && let Some(px) = raw.strip_suffix("(automatic)")
        {
            let px = px.trim();
            let reading = px
                .parse::<f64>()
                .ok()
                .and_then(|value| crate::prefs::numeric_reading(key, value, px))
                .unwrap_or_else(|| px.to_string());
            format!("{reading} (automatic)")
        } else if key == crate::prefs::EDIT_WALLPAPER {
            raw.trim().to_string()
        } else if crate::prefs::edit_kind(key) == crate::prefs::EditKind::Color
            && let Some(color) = aterm_types::scheme::parse_hex(raw)
        {
            format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
        } else if let Some(reading) = raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .and_then(|value| crate::prefs::numeric_reading(key, value, raw.trim()))
        {
            reading
        } else {
            friendly_preview_value(raw)
        };
        Some((subject.to_string(), value))
    }

    fn candidate_is_unsaved(&self) -> bool {
        (self.theme_sample && self.focused_key == crate::prefs::EDIT_THEME)
            || self.trying_style().is_some()
    }

    fn candidate_badge(&self) -> String {
        self.candidate_parts().map_or_else(
            || friendly_preview_value(&self.focused_value),
            |(subject, value)| format!("{subject}: {value}"),
        )
    }
}

const TYPOGRAPHY_KEPT_ROWS: usize = 5;

const TYPOGRAPHY_SPECIMEN_ROWS: usize = 10;

const PREVIEW_INSET: f32 = 10.0;

fn header_caption() -> crate::type_scale::StepPx {
    TypeStep::Caption
        .px(13.75)
        .scaled(crate::native_appearance::text_scale())
}

/// The band of a preview header that paints no words: the room its Pause/Play glyph takes, and
/// the least every host's preview height counts on.
const HEADER_BAND: f32 = 27.0;

/// A header's first line: its caption with 4pt over and under it at every text size, never
/// under [`HEADER_BAND`].
fn header_first_line(px: f32) -> f32 {
    HEADER_BAND.max(px + 8.0)
}

/// The height of a header with `lines` lines of its `px` caption: the band alone without words,
/// else the first line and a pitch for each further line.
fn header_height_for(px: f32, lines: usize) -> f32 {
    match lines {
        0 => HEADER_BAND,
        lines => header_first_line(px) + (lines - 1) as f32 * header_line_pitch(px),
    }
}

fn header_line_pitch(px: f32) -> f32 {
    (px * 1.3).ceil()
}

/// A preview header laid out for one width ([`SettingsPreviewSpec::header_layout`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewHeader {
    /// The scene label leads the first line.
    pub(crate) scene: bool,
    /// The state's lines, top to bottom.
    pub(crate) lines: Vec<String>,
    /// What the lines say together.
    pub(crate) says: String,
    /// The room each line has, left of the Pause/Play control's slot.
    pub(crate) available: f32,
    /// The caption's size.
    pub(crate) px: f32,
    /// The header band's height, down to the terminal body's gap.
    pub(crate) height: f32,
}

impl PreviewHeader {
    /// The band alone: no scene label and no statement.
    fn without_words(self) -> Self {
        Self {
            scene: false,
            lines: Vec::new(),
            says: String::new(),
            height: HEADER_BAND,
            ..self
        }
    }

    /// The height of the first line, which the scene label, the state and the Pause/Play
    /// glyph centre in.
    fn first_line(&self) -> f32 {
        header_height_for(self.px, self.lines.len().min(1))
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
    let Some(pill) = spec.pill_layout(terminal.width, text_scale) else {
        return;
    };
    let text_size = TypeStep::Caption.px(11.5).scaled(text_scale);
    let pill_h = pill.height;
    if terminal.height < pill_h + 8.0 {
        return;
    }
    let warning = pill.warning;
    let text_width = pill
        .lines
        .iter()
        .map(|line| crate::tray_raster::ui_text_width_for(TextFace::UiBold, line, pill.px))
        .fold(0.0_f32, f32::max);
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
    let first = crate::tray_raster::row_baseline(
        y,
        font_candidate_pill_height(text_scale),
        text_size.get(),
    );
    let pitch = (pill.px * 1.3).ceil();
    for (index, line) in pill.lines.into_iter().enumerate() {
        prims.push(text_prim(
            x + 8.0,
            first + index as f32 * pitch,
            line,
            text_size,
            TextWeight::Regular,
            TextFace::UiBold,
            rgba(roles.text_primary, 255),
        ));
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PillLayout {
    /// Its words, top to bottom.
    lines: Vec<String>,
    /// The statement those lines say.
    says: String,
    /// It warns that the font failed.
    warning: bool,
    /// The caption's size.
    px: f32,
    /// The room a line has.
    budget: f32,
    /// The pill's height: one line's, and a line pitch for each further one.
    height: f32,
}

/// One run of words a preview paints itself, as the fit audit measures it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewTextRun {
    pub(crate) says: String,
    pub(crate) painted: String,
    pub(crate) required: f32,
    pub(crate) available: f32,
}

fn paint_platform_window_controls(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    title_h: f32,
    ink: [u8; 3],
) -> f32 {
    #[cfg(target_os = "macos")]
    {
        let _ = ink;
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
            color: rgba(ink, 255),
        });
        22.0
    }
}

const fn window_chrome_text_color(light: bool) -> [u8; 3] {
    if light {
        [0x64, 0x68, 0x72]
    } else {
        [0xB9, 0xBD, 0xC9]
    }
}

const WINDOW_SAMPLE_WEARS_THE_BAND: bool = cfg!(windows);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowSampleTone {
    /// The title bar's fill.
    fill: [u8; 3],
    /// The title line and the window control, 4.5:1 or more on `fill`.
    ink: [u8; 3],
    /// The 1-px rim round the bar, where its fill alone would not stand off the specimen.
    edge: Option<[u8; 3]>,
}

const WINDOW_SAMPLE_RIM_BELOW: f64 = 1.3;

fn window_sample_tone(
    spec: &SettingsPreviewSpec,
    candidate: &PreviewTerminalTheme,
    light: bool,
) -> WindowSampleTone {
    use aterm_messages::ink::{contrast, ensure_contrast, mix3};
    let auto = !matches!(spec.appearance.window_theme.as_str(), "light" | "dark");
    let page = crate::settings::u32_rgb(candidate.bg);
    let ink_on = |fill: [u8; 3]| ensure_contrast(window_chrome_text_color(light), fill, 4.5);
    let indistinct = |fill: [u8; 3]| contrast(fill, page) < WINDOW_SAMPLE_RIM_BELOW;
    if WINDOW_SAMPLE_WEARS_THE_BAND && auto {
        let strip = crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
            theme: candidate.into_theme(),
            ansi: Some(demo_ansi_hues(candidate)),
            authored: spec.demo.chrome,
        });
        return WindowSampleTone {
            fill: strip.band,
            ink: ink_on(strip.band),
            edge: indistinct(strip.band).then_some(strip.seam),
        };
    }
    let fill = if light {
        [0xF1, 0xF2, 0xF5]
    } else {
        [0x20, 0x22, 0x2A]
    };
    let ink = ink_on(fill);
    let seam = ensure_contrast(
        mix3(fill, ink, aterm_messages::tokens::SEAM_T),
        fill,
        aterm_messages::tokens::SEAM_FLOOR,
    );
    WindowSampleTone {
        fill,
        ink,
        edge: indistinct(fill).then(|| ensure_contrast(seam, page, 3.0)),
    }
}

fn theme_bg_is_dark(bg: u32) -> bool {
    aterm_render::theme_is_dark(bg)
}

fn window_sample_light(spec: &SettingsPreviewSpec, terminal_theme: Theme) -> bool {
    match spec.appearance.window_theme.as_str() {
        "light" => true,
        "dark" => false,
        _ => !theme_bg_is_dark(terminal_theme.bg),
    }
}

const APPEARANCE_SPECIMEN_ROWS: usize = 16;
const APPEARANCE_SWATCH_BOTTOM_INSET: f32 = 10.0;
const APPEARANCE_SWATCH_HEIGHT: f32 = 6.0;
const APPEARANCE_SPECIMEN_SWATCH_GAP: f32 = 4.0;
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

fn appearance_terminal_floor(row: f32) -> f32 {
    (row + APPEARANCE_SPECIMEN_SWATCH_GAP).max(APPEARANCE_SWATCH_TOP_CLEARANCE)
        + APPEARANCE_SWATCH_BOTTOM_INSET
}

fn appearance_whole_rows(terminal: LogicalRect, drop: f32, row: f32) -> usize {
    if terminal.height < appearance_terminal_floor(row) {
        return 0;
    }
    (((appearance_specimen_clip(terminal).height - drop) / row)
        .floor()
        .max(0.0) as usize)
        .min(APPEARANCE_SPECIMEN_ROWS)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowBand {
    bottom: f32,
    text_room: f32,
}

const APPEARANCE_WINDOW_SAMPLE_GAP: f32 = 6.0;

const fn appearance_window_sample_width(terminal: LogicalRect) -> f32 {
    terminal.width.min(112.0)
}

fn appearance_window_sample_drawn(terminal: LogicalRect) -> bool {
    terminal.height >= APPEARANCE_WINDOW_SAMPLE_HEIGHT + APPEARANCE_SWATCH_BOTTOM_INSET
}

fn appearance_window_band(terminal: LogicalRect) -> Option<WindowBand> {
    appearance_window_sample_drawn(terminal).then(|| WindowBand {
        bottom: APPEARANCE_WINDOW_SAMPLE_HEIGHT,
        text_room: terminal.width
            - appearance_window_sample_width(terminal)
            - APPEARANCE_WINDOW_SAMPLE_GAP,
    })
}

fn appearance_row_columns(spec: &SettingsPreviewSpec) -> Vec<usize> {
    let mut columns = APPEARANCE_SOURCE
        .split("\r\n")
        .map(|line| {
            let mut visible = String::new();
            let mut chars = line.chars();
            while let Some(ch) = chars.next() {
                if ch == '\u{1b}' {
                    for byte in chars.by_ref() {
                        if byte.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    visible.push(ch);
                }
            }
            visible.trim_end().chars().count()
        })
        .collect::<Vec<_>>();
    if let Some(second) = columns.get_mut(1) {
        *second = (*second).max(specimen_cursor_col(spec) + 1);
    }
    columns
}

fn paint_window_theme_sample(
    prims: &mut Vec<DrawPrim>,
    terminal: LogicalRect,
    spec: &SettingsPreviewSpec,
    terminal_theme: Theme,
) {
    let candidate = spec
        .terminal_theme
        .unwrap_or_else(|| PreviewTerminalTheme::over_engine_palette(terminal_theme));
    let light = window_sample_light(spec, terminal_theme);
    let width = appearance_window_sample_width(terminal);
    let x = terminal.right() - width;
    let tone = window_sample_tone(spec, &candidate, light);
    if appearance_window_sample_drawn(terminal) {
        prims.push(DrawPrim::Panel {
            x,
            y: terminal.y,
            w: width,
            h: APPEARANCE_WINDOW_SAMPLE_HEIGHT,
            radius: 0.0,
            fill: rgba(tone.fill, 255),
        });
        if let Some(edge) = tone.edge {
            prims.push(DrawPrim::Stroke {
                x,
                y: terminal.y,
                w: width,
                h: APPEARANCE_WINDOW_SAMPLE_HEIGHT,
                radius: 0.0,
                width: 1.0,
                color: rgba(edge, 255),
            });
        }
        let title_inset = paint_platform_window_controls(
            prims,
            x,
            terminal.y,
            APPEARANCE_WINDOW_SAMPLE_HEIGHT,
            tone.ink,
        );
        let line_y = terminal.y + (APPEARANCE_WINDOW_SAMPLE_HEIGHT / 2.0).round();
        prims.push(DrawPrim::Line {
            x1: x + title_inset,
            y1: line_y,
            x2: terminal.right() - 8.0,
            y2: line_y,
            width: 2.0,
            color: rgba(tone.ink, 255),
        });
    }

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
    let swatch_count = candidate.ansi.len() as f32;
    let swatch_room = (terminal.width - 16.0).max(0.0);
    let swatch_gap = if swatch_room >= swatch_count * 4.0 {
        2.0
    } else {
        0.0
    };
    let swatch_width = (swatch_room - swatch_gap * (swatch_count - 1.0)) / swatch_count;
    let swatch_y = appearance_swatch_y(terminal);
    if swatch_y < terminal.y + APPEARANCE_SWATCH_TOP_CLEARANCE {
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

/// The hues the chrome borrows from a candidate's palette (its red, green, yellow, blue and cyan).
pub(crate) fn demo_ansi_hues(candidate: &PreviewTerminalTheme) -> aterm_messages::ink::AnsiHues {
    let rgb = crate::settings::u32_rgb;
    aterm_messages::ink::AnsiHues {
        red: rgb(candidate.ansi[1]),
        green: rgb(candidate.ansi[2]),
        yellow: rgb(candidate.ansi[3]),
        blue: rgb(candidate.ansi[4]),
        cyan: rgb(candidate.ansi[6]),
    }
}

/// `color` packed as `0x00RRGGBB`.
pub(crate) const fn pack_rgb(color: aterm_types::Rgb) -> u32 {
    ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
}

const fn packed_rgb(value: u32) -> aterm_types::Rgb {
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
    build_terminal_specimen_input_grid(spec, theme, visible_rows, None)
}

fn build_terminal_specimen_input_grid(
    spec: &SettingsPreviewSpec,
    theme: Theme,
    visible_rows: Option<usize>,
    grid: Option<(usize, usize)>,
) -> (Arc<aterm_render::RenderInput>, u64) {
    use std::hash::{Hash, Hasher};

    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::{CursorStyle, Terminal};

    const FULL_SOURCE: &str = concat!(
        "\u{1b}[34mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
        "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
        "\u{1b}[4mgypq_j underline\u{1b}[0m\r\n",
        "CJK 你好  symbols ∑✓♥\r\n",
        "emoji 😀 🚀 👩‍💻\r\n",
        "0O 1lI| {}[]() ;:,.\r\n",
        "<= >= === !== |> :: ->>\r\n",
        "┌─┬─┐ ▁▂▃▅▇█ ░▒▓\r\n",
        "Ünïcödé àéîõ ß ﬁ\r\n",
        "0123456789 $%&@#~^"
    );
    let cursor_scene = spec.scene == PreviewScene::CursorMotion;
    let (rows, source) = match spec.scene {
        PreviewScene::Appearance | PreviewScene::ThemeDemo | PreviewScene::Wallpaper => {
            (APPEARANCE_SPECIMEN_ROWS, APPEARANCE_SOURCE.to_string())
        }
        PreviewScene::Typography => (TYPOGRAPHY_SPECIMEN_ROWS, FULL_SOURCE.to_string()),
        PreviewScene::CursorMotion => {
            let shown = visible_rows
                .unwrap_or(CURSOR_SPECIMEN_ROWS)
                .clamp(1, CURSOR_SPECIMEN_ROWS);
            let lines = cursor_scene_lines(shown);
            (
                lines + 1,
                CURSOR_SPECIMEN_LINES[CURSOR_SPECIMEN_LINES.len() - lines..].join("\r\n"),
            )
        }
    };
    let rows = if cursor_scene {
        rows
    } else {
        visible_rows.unwrap_or(rows).clamp(1, rows)
    };
    let source = source
        .match_indices("\r\n")
        .nth(rows - 1)
        .map_or(source.as_str(), |(end, _)| &source[..end]);
    let own_cols = if cursor_scene {
        CURSOR_SCENE_COLS
    } else {
        SPECIMEN_COLS
    };
    let (grid_rows, cols) = grid.map_or((rows, own_cols), |(grid_rows, cols)| {
        (grid_rows.clamp(rows, 256), cols.clamp(1, 512))
    });

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    rows.hash(&mut hash);
    (grid_rows, cols).hash(&mut hash);
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
    spec.appearance
        .indexed_minimum_contrast
        .to_bits()
        .hash(&mut hash);
    (spec.cursor.style as u8).hash(&mut hash);
    spec.cursor.blink.hash(&mut hash);
    (!cursor_scene && spec.cursor_visible()).hash(&mut hash);
    specimen_cursor_col(spec).hash(&mut hash);
    let fingerprint = hash.finish() | 1;

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
        indexed_minimum_contrast: spec.appearance.indexed_minimum_contrast,
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
    let mut terminal = Terminal::new(grid_rows as u16, cols as u16);
    let _ = terminal.apply_config(&terminal_config);
    terminal.process(source.as_bytes());
    let mut input = terminal.cell_frame(grid_rows, cols);
    input.cursor_style = terminal_config.cursor_style;
    input.cursor_color = theme.cursor;
    input.default_bg = theme.bg;
    if cursor_scene {
        input.cursor_row = prompt_row(&input);
        input.cursor_col = 0;
        input.cursor_visible = false;
    } else {
        input.cursor_row = 1.min(rows - 1);
        input.cursor_col = specimen_cursor_col(spec);
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

const APPEARANCE_SOURCE: &str = if cfg!(windows) {
    APPEARANCE_SOURCE_PWSH
} else {
    APPEARANCE_SOURCE_SH
};

const APPEARANCE_SOURCE_SH: &str = concat!(
    "\u{1b}[34mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
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

const APPEARANCE_SOURCE_PWSH: &str = concat!(
    "\u{1b}[34mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
    "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
    "\u{1b}[4mgypq_j underline\u{1b}[0m\r\n",
    "\u{1b}[1;34mdocs\u{1b}[0m  \u{1b}[1;32mbuild.ps1\u{1b}[0m  \u{1b}[35mlogo.png\u{1b}[0m  ",
    "\u{1b}[36mlink\u{1b}[0m\r\n",
    "\u{1b}[31merror\u{1b}[0m \u{1b}[33mwarning\u{1b}[0m \u{1b}[32mpassed\u{1b}[0m \u{1b}[34mnote\u{1b}[0m\r\n",
    "PS C:\\src> \u{1b}[93mgit\u{1b}[0m status \u{1b}[90m--short\u{1b}[0m\r\n",
    "\u{1b}[32mM\u{1b}[0m  src/theme.rs\r\n",
    " \u{1b}[31mM\u{1b}[0m README.md\r\n",
    "\u{1b}[31m??\u{1b}[0m notes.txt\r\n",
    "PS C:\\src> \u{1b}[93mgit\u{1b}[0m diff\r\n",
    "\u{1b}[36m@@ -8,2 +8,2 @@\u{1b}[0m fn main()\r\n",
    "\u{1b}[31m-    let x = 1;\u{1b}[0m\r\n",
    "\u{1b}[32m+    let x = 2;\u{1b}[0m\r\n",
    "PS C:\\src> \u{1b}[93mcargo\u{1b}[0m test\r\n",
    "\u{1b}[32mok\u{1b}[0m 42 passed  \u{1b}[33m1 ignored\u{1b}[0m\r\n",
    "PS C:\\src> "
);

const SPECIMEN_COLS: usize = 32;

fn specimen_cursor_col(spec: &SettingsPreviewSpec) -> usize {
    if spec.focused_key == crate::prefs::EDIT_CURSOR_BREAK_LIGATURES {
        1
    } else {
        "!= => -> ITAL B+I cursor".len()
    }
}

const CURSOR_SPECIMEN_ROWS: usize = CURSOR_SPECIMEN_LINES.len() + 1;

const fn cursor_scene_lines(shown: usize) -> usize {
    if shown >= 3 { shown - 1 } else { shown }
}

fn prompt_row(input: &RenderInput) -> usize {
    input
        .cells
        .iter()
        .rposition(|row| row.iter().any(|cell| cell.ch != ' '))
        .unwrap_or(input.rows.saturating_sub(1))
}

/// What the scene's command prints once the Return hands it to the shell: `git commit`'s
/// summary, the commit naming what `cat README.md` above read. It is shorter than the Tab's
/// landing column, so the next prompt's Tab lands with blank cells over it, where a dark landing
/// ring shows whole by the letter guard's law (`RIM_SHOWN_SHARE`) as the terminal's does: under
/// the command itself, every landing of the scene lay among the letters of the line above, and
/// on dark Ring on jumps showed as the landing's veil alone (audit r14 PV7).
const CURSOR_SCENE_OUTPUT: &str = "[main 4f2c9e1] ship it";

/// The rows a Return scrolls the scene by: the line it hands the shell and the line the shell
/// prints ([`CURSOR_SCENE_OUTPUT`]).
const CURSOR_SCENE_RETURN_ROWS: u16 = 2;

/// `base` once the Return has handed the prompt row's line to the shell: the line and what the
/// shell printed for it ([`CURSOR_SCENE_OUTPUT`]) lie over the fresh prompt, the rows above
/// scrolled up to make room.
fn scroll_submitted_line(base: &RenderInput, row: usize) -> RenderInput {
    let mut scrolled = base.clone();
    if row == 0 || row >= scrolled.cells.len() {
        return scrolled;
    }
    let submitted = scrolled.cells[row].clone();
    // A cell in the page's own colours: the command's first, past the
    // prompt's coloured path.
    let Some(&blank) = submitted.get(TypedLine::of(&submitted).start) else {
        return scrolled;
    };
    let blank = aterm_core::terminal::RenderCell { ch: ' ', ..blank };
    let mut printed = vec![blank; submitted.len()];
    for (cell, ch) in printed.iter_mut().zip(CURSOR_SCENE_OUTPUT.chars()) {
        cell.ch = ch;
    }
    let lines = [submitted, printed];
    let shift = lines.len().min(row);
    scrolled.cells[..row].rotate_left(shift);
    for (slot, line) in scrolled.cells[row - shift..row]
        .iter_mut()
        .zip(&lines[lines.len() - shift..])
    {
        slot.clone_from(line);
    }
    scrolled
}

const CURSOR_SPECIMEN_LINES: [&str; 7] = if cfg!(windows) {
    CURSOR_SPECIMEN_LINES_PWSH
} else {
    CURSOR_SPECIMEN_LINES_SH
};

const CURSOR_SPECIMEN_LINES_SH: [&str; 7] = [
    "\u{1b}[32m~/demo\u{1b}[0m $ git pull",
    "Already up to date.",
    "\u{1b}[32m~/demo\u{1b}[0m $ ls",
    "\u{1b}[1;34msrc\u{1b}[0m  README.md  \u{1b}[1;32mbuild.sh\u{1b}[0m",
    "\u{1b}[32m~/demo\u{1b}[0m $ cat README.md",
    "ship it \u{2713}",
    "\u{1b}[32m~/demo\u{1b}[0m $ git add README.md && git commit",
];

const CURSOR_SPECIMEN_LINES_PWSH: [&str; 7] = [
    "PS C:\\> \u{1b}[93mgit\u{1b}[0m pull",
    "Already up to date.",
    "PS C:\\> \u{1b}[93mgit\u{1b}[0m status \u{1b}[90m-s\u{1b}[0m",
    " \u{1b}[31mM\u{1b}[0m README.md",
    "PS C:\\> \u{1b}[93mcat\u{1b}[0m README.md",
    "ship it \u{2713}",
    "PS C:\\> \u{1b}[93mgit\u{1b}[0m add .\\README.md; \u{1b}[93mgit\u{1b}[0m commit",
];

const KEY_MS: u64 = 3 * MOTION_STEP_MS;

const CURSOR_SCENE_COMMAND: &str = if cfg!(windows) {
    "git add .\\README.md; git commit"
} else {
    "git add README.md && git commit"
};

/// The typed cells and where the Tab lands: `R` completes `README.md`, as `.\R` completes
/// `.\README.md` in PowerShell, so both scenes jump 8 cells and keep the same time: a leap, as
/// ordinary a completion as there is, that Fire flies as a meteor Trail length cuts, over the
/// letters a shorter hop's flame would leave dark.
const CURSOR_SCENE_TAB: (usize, usize) = if cfg!(windows) { (11, 19) } else { (9, 17) };

const CURSOR_SCENE_KEYS: u64 =
    (CURSOR_SCENE_TAB.0 + 1 + (CURSOR_SCENE_COMMAND.len() - CURSOR_SCENE_TAB.1)) as u64;

/// One licensed jump the cursor scene makes: `rows` rows and `cols` columns, over typed letters
/// only or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SceneJump {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) over_letters: bool,
}

/// The scene's jumps: the Tab's completion along the typed line, over the letters it completes,
/// and the Return from the line's end down past what the shell prints to the next prompt's first
/// column.
pub(crate) const fn scene_jumps() -> [SceneJump; 2] {
    [
        SceneJump {
            rows: 0,
            cols: (CURSOR_SCENE_TAB.1 - CURSOR_SCENE_TAB.0) as u16,
            over_letters: true,
        },
        SceneJump {
            rows: CURSOR_SCENE_RETURN_ROWS,
            cols: CURSOR_SCENE_COMMAND.len() as u16,
            over_letters: false,
        },
    ]
}

const CURSOR_SCENE_HOLD_MS: u64 = 15 * MOTION_STEP_MS;

const CURSOR_SCENE_CYCLE_MS: u64 = CURSOR_SCENE_KEYS * KEY_MS + CURSOR_SCENE_HOLD_MS;
const _: () = assert!(CURSOR_SCENE_CYCLE_MS.is_multiple_of(MOTION_STEP_MS));

const CURSOR_SCENE_PAUSE_MS: u64 = (CURSOR_SCENE_TAB.0 as u64 + 1) * KEY_MS + MOTION_STEP_MS;
const _: () = assert!(CURSOR_SCENE_PAUSE_MS < CURSOR_SCENE_KEYS * KEY_MS);

const CURSOR_SCENE_WARM_CYCLES: u64 = 2;

const CURSOR_SCENE_COLS: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneEvent {
    /// A fresh prompt: the Return that handed the last line to the shell.
    Return,
    /// The `n`th printable key of the command (0-based cell).
    Glyph(usize),
    /// The Tab that completes [`CURSOR_SCENE_TAB`].
    Tab,
}

impl SceneEvent {
    fn at(offset: u64) -> Option<Self> {
        if offset == 0 {
            return Some(Self::Return);
        }
        if !offset.is_multiple_of(KEY_MS) {
            return None;
        }
        let key = usize::try_from(offset / KEY_MS).ok()?;
        let (from, to) = CURSOR_SCENE_TAB;
        match key {
            0 => None,
            key if key <= from => Some(Self::Glyph(key - 1)),
            key if key == from + 1 => Some(Self::Tab),
            key if key as u64 <= CURSOR_SCENE_KEYS => Some(Self::Glyph(to + key - from - 2)),
            _ => None,
        }
    }
}

fn scene_cells_at(offset: u64) -> usize {
    let (from, to) = CURSOR_SCENE_TAB;
    let keys = usize::try_from((offset / KEY_MS).min(CURSOR_SCENE_KEYS)).unwrap_or(0);
    if keys <= from {
        keys
    } else {
        to + (keys - from - 1)
    }
}

#[derive(Clone, Copy, Debug)]
struct TypedLine {
    /// First column after the prompt.
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
            .position(|pair| matches!(pair[0].ch, '$' | '>') && pair[1].ch == ' ')
            .map_or(0, |mark| mark + 2)
            .min(end);
        Self { start, end }
    }

    fn shown_at(self, offset: u64) -> usize {
        scene_cells_at(offset).min(self.end - self.start)
    }

    const fn caret(self, shown: usize) -> usize {
        self.start + shown
    }
}

/// Tests' observation and negative controls over the cursor scenes a thread builds
/// ([`CursorScene::new`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SceneControls {
    /// The pet seats off the letters ([`scene_pet_seat`]); off, it stands at
    /// the station beside the caret wherever its body lies.
    pet_seat_law: bool,
    /// The trail reads the frame's text; off, it reads none, as the scene's
    /// step fed it none before.
    frame_text: bool,
    /// A particle the card's edge would cut is left out whole; off, it is
    /// drawn cut (`CursorGlow::set_motes_cut`).
    motes_whole: bool,
    /// The pet keeps one seat for the line it is written beside
    /// ([`scene_pet_seat`]'s `kept`); off, each key seats it afresh.
    one_seat_a_line: bool,
    /// The trail reads what the card's face lays of each glyph
    /// ([`SceneStage::ink`]); off, it reads every glyph's ink as its colour.
    glyph_ink: bool,
    /// A light rising past the card's top edge is laid shorter inside it;
    /// off, the box's clip cuts it flat there (`CursorGlow::set_rise_cut`).
    rises_kept: bool,
    /// The card draws its caret in `CursorFxFrame::caret_fill` (a bar's
    /// lifted colour); off, in the block's fill alone, as the card did before.
    thin_caret_fill: bool,
    /// Retain the actual pre-yield body's ablated under stream with its cached step.
    observe_body: bool,
    /// A typed light lives its tuned life times the duration's share of the default
    /// (`CursorGlow::set_typed_life_scaled`); off, its tuned life whatever the duration.
    typed_life_scaled: bool,
    /// The letter guard keeps each letter's whole reading as well as its ring's; off, the ring's
    /// alone (`CursorGlow::set_letter_guard_whole`).
    letter_whole: bool,
    /// The letter guard holds each page shade (Beam's body on a dim page) to the letters it
    /// reaches; off, each is kept whole (`CursorGlow::set_letter_guard_shades`).
    shades_held: bool,
    /// The letter guard passes by a step the card does not keep
    /// (`CursorGlow::set_frame_unseen`); off, it judges every step, as a window's frames.
    unseen_steps_unguarded: bool,
}

#[cfg(test)]
thread_local! {
    static SCENE_CONTROLS: std::cell::Cell<SceneControls> = const {
        std::cell::Cell::new(SceneControls {
            pet_seat_law: true,
            frame_text: true,
            motes_whole: true,
            one_seat_a_line: true,
            glyph_ink: true,
            rises_kept: true,
            thin_caret_fill: true,
            observe_body: false,
            typed_life_scaled: true,
            letter_whole: true,
            shades_held: true,
            unseen_steps_unguarded: true,
        })
    };
}

/// The scene's lettered cells as shown with the typed line's caret at `caret`:
/// `cells` hold the whole command on the prompt row, whose cells from the caret
/// to the command's end are not typed yet.
#[derive(Clone, Copy)]
struct SceneLetters<'a> {
    cells: &'a [Vec<aterm_core::terminal::RenderCell>],
    /// The prompt's row.
    row: usize,
    line: TypedLine,
    caret: usize,
}

impl SceneLetters<'_> {
    /// Whether the cell at `(row, col)` holds a glyph (a blank holds none).
    fn lettered(&self, row: usize, col: usize) -> bool {
        if row == self.row && col >= self.caret && col < self.line.end {
            return false;
        }
        self.cells
            .get(row)
            .and_then(|cells| cells.get(col))
            .is_some_and(|cell| !matches!(cell.ch, ' ' | '\0'))
    }
}

/// What the resident pet's frame reads ([`CursorScene::place_companion`]).
struct PetCue<'a> {
    letters: SceneLetters<'a>,
    /// The caret's column after the loop's Return and after each of its keys,
    /// in order (empty for a still scene).
    carets: Vec<usize>,
    /// The keys the loop has typed: `carets[keys]` is the caret shown.
    keys: usize,
    /// Scene time since the loop's Return, ms.
    offset: u64,
    /// The typed cells, which step the walk.
    typed: usize,
}

/// How many rows under the caret's the pet's seat may lie on (the terminal
/// head law's `HEAD_SEAT_ROWS`).
const PET_SEAT_ROWS: u16 = 3;

/// A resident pet's frame whose body's left edge stands `seat.0` columns
/// along row `seat.1`, `width` columns wide at its own size, drawn at `fit`.
fn scene_pet_frame(
    (left, row): (f32, f32),
    (width, fit): (f32, f32),
    (action, pose): (
        aterm_effects::kitty_pet::PetAction,
        aterm_effects::pet_glyphs_gen::PetGlyphId,
    ),
) -> aterm_effects::kitty_pet::PetFrame {
    use aterm_effects::kitty_pet::{PET_DEPARTURES_MAX, PET_MOTES_MAX, PetFrame};

    PetFrame {
        alpha: 255,
        lane_alpha: 255,
        action,
        pose,
        // `PetFrame::body_px` centres the drawn body on the art's own width.
        col: left - width * (1.0 - fit) / 2.0,
        row,
        lift: 0.0,
        facing_left: false,
        scale_x: fit,
        scale_y: fit,
        stretch_from: aterm_effects::kitty_pet::StretchFrom::Centre,
        purr: 0.0,
        contented: false,
        under_ink: false,
        motes: [None; PET_MOTES_MAX],
        departures: [None; PET_DEPARTURES_MAX],
    }
}

/// The box a pet seated at `seat` draws, in grid pixels, end exclusive: the
/// sprite `pet_cursor` lays (`PetFrame::body_px`), held over the card's
/// `floor` as [`CursorScene::place_companion`] holds it.
fn scene_pet_box(
    seat: (f32, f32),
    size: (f32, f32),
    geom: aterm_effects::word_decorations::EffectGeom,
    floor: i32,
) -> Option<(i32, i32, i32, i32)> {
    use aterm_effects::kitty_pet::PetAction;
    use aterm_effects::pet_glyphs_gen::PetGlyphId;

    let (x0, x1, y0, y1) = scene_pet_frame(seat, size, (PetAction::Sit, PetGlyphId::PetSit))
        .body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)?;
    let y = y0.min(floor - (y1 - y0)).max(0);
    Some((x0, x1, y, y + (y1 - y0)))
}

/// Whether a pet's drawn `body` keeps off every lettered cell and the caret's.
fn scene_pet_clear(
    (x0, x1, y0, y1): (i32, i32, i32, i32),
    letters: SceneLetters<'_>,
    caret: (u16, u16),
    geom: aterm_effects::word_decorations::EffectGeom,
) -> bool {
    let (cw, ch) = (i32::from(geom.cell_w), i32::from(geom.cell_h));
    if cw <= 0 || ch <= 0 || x1 <= x0 || y1 <= y0 {
        return true;
    }
    let cells = |lo: i32, hi: i32, size: i32| {
        let first = usize::try_from(lo.max(0) / size).unwrap_or(0);
        let last = usize::try_from((hi - 1).max(0) / size).unwrap_or(0);
        first..=last
    };
    let caret = (usize::from(caret.0), usize::from(caret.1));
    cells(y0, y1, ch)
        .all(|row| cells(x0, x1, cw).all(|col| (row, col) != caret && !letters.lettered(row, col)))
}

/// Where the resident pet sits for a caret at `caret`, as the terminal's head
/// law seats it (`kitty_pet`'s `clear_of_letters_above`), its whole drawn body
/// off the letters and the caret: the station beside the caret where its body
/// lies on neither; else the nearest seat, a row counting as two columns as the
/// law counts it, along the caret's row away from the caret (past the letters
/// over its head) or on a shown row under the caret's ([`PET_SEAT_ROWS`]: the
/// blank row under the prompt). With no seat the station holds. The left edge
/// of the drawn body, in columns, and its row; `size` is the art's own width
/// in columns and the share it is drawn at, `floor` the card's shown height
/// (px). A pet `kept` on a row under the caret's while the line is written
/// keeps that row while it holds a seat, the nearest along it to the station
/// (the head law's ONE SEAT FOR THE WHOLE LINE): taken up beside the caret
/// once the writing passed the line above's end, it flew up mid-line and down
/// again at the Return.
fn scene_pet_seat(
    letters: SceneLetters<'_>,
    caret: (u16, u16),
    geom: aterm_effects::word_decorations::EffectGeom,
    size: (f32, f32),
    (floor, kept): (i32, Option<(f32, f32)>),
) -> (f32, f32) {
    /// The step (columns) a seat is sought along a row by.
    const STEP: f32 = 1.0 / 8.0;
    /// A row's distance in columns (the head law's `ROW_AS_COLS`).
    const ROW_AS_COLS: f32 = 2.0;

    let width = size.0 * size.1;
    let limit = (f32::from(geom.cols) - width).max(0.0);
    let station = aterm_effects::kitty_pet::PetBrain::station(caret.1, geom.cols, width);
    let clear = |left: f32, row: f32| {
        scene_pet_box((left, row), size, geom, floor)
            .is_some_and(|body| scene_pet_clear(body, letters, caret, geom))
    };
    let row = f32::from(caret.0);
    let steps = |from: f32, toward: f32| {
        (0_u16..)
            .map(move |step| from + f32::from(step) * toward)
            .take_while(move |left| (0.0..=limit).contains(left))
    };
    let shown = u16::try_from(floor / i32::from(geom.cell_h.max(1))).unwrap_or(0);
    if let Some((_, kept_row)) = kept {
        let below = kept_row - row;
        if (1.0..=f32::from(PET_SEAT_ROWS)).contains(&below)
            && kept_row < f32::from(shown.min(geom.rows))
        {
            let nearest = [STEP, -STEP]
                .into_iter()
                .filter_map(|toward| steps(station, toward).find(|&left| clear(left, kept_row)))
                .min_by(|a, b| (a - station).abs().total_cmp(&(b - station).abs()));
            if let Some(left) = nearest {
                return (left, kept_row);
            }
        }
    }
    if clear(station, row) {
        return (station, row);
    }
    let away = if station >= f32::from(caret.1) {
        STEP
    } else {
        -STEP
    };
    let mut best = steps(station, away)
        .find(|&left| clear(left, row))
        .map(|left| ((left - station).abs(), left, row));
    for below in 1..=PET_SEAT_ROWS {
        let under = caret.0.saturating_add(below);
        let drop = ROW_AS_COLS * f32::from(below);
        if under >= shown.min(geom.rows) || best.is_some_and(|(d, ..)| drop >= d) {
            break;
        }
        let under = f32::from(under);
        for toward in [STEP, -STEP] {
            let Some(left) = steps(station, toward).find(|&left| clear(left, under)) else {
                continue;
            };
            let d = (left - station).hypot(drop);
            if best.is_none_or(|(bd, ..)| d < bd) {
                best = Some((d, left, under));
            }
        }
    }
    best.map_or((station, row), |(_, left, seat)| (left, seat))
}

/// What one cursor-scene card showed: its phase, its cells, its decorated input (the text,
/// the caret and the lights it played) and its companion's drawn body. A window's capture keeps
/// the card its frame painted ([`stash_captured_scene_card`]), and `settings card` names it as
/// the terminal's `dims`, `cursor`, `text` and `trail status` replies name a terminal frame
/// ([`captured_scene_card_reply`]): the trail gallery keeps those beside each settings frame,
/// so `aterm-lab trail-contrast` and `trail-companion` judge the demo's letters and companion
/// as they judge the terminal's.
#[derive(Clone, Debug)]
pub(crate) struct SceneCardShot {
    phase_ms: u64,
    /// Device pixels.
    cell: (usize, usize),
    input: RenderInput,
    /// Card device pixels `(x0, x1, y0, y1)`, end exclusive: the union of the companion's
    /// sprites as the card drew them (the terminal's `pet_body`, its sprite unshaved).
    body: Option<(i32, i32, i32, i32)>,
    #[cfg(test)]
    without_body_under: Option<Vec<GlowQuad>>,
    #[cfg(test)]
    scene_fingerprint: u64,
}

/// The actual last decorated card, removing only its recorded body glass before the yield.
#[cfg(test)]
pub(crate) fn scene_card_without_body_for_test(
    observed: &RenderInput,
    scene_fingerprint: u64,
) -> Option<RenderInput> {
    DECORATED_CARD.with_borrow(|shot| {
        let shot = shot.as_ref()?;
        if shot.scene_fingerprint != scene_fingerprint || shot.input != *observed {
            return None;
        }
        let mut input = shot.input.clone();
        input
            .glow_under
            .clone_from(shot.without_body_under.as_ref()?);
        Some(input)
    })
}

/// A card a raster blitted: the shot, the specimen it played on (for its bare twin) at the
/// scale it rendered, and its top-left in the raster's device pixels.
#[derive(Clone)]
struct PaintedSceneCard {
    shot: std::rc::Rc<SceneCardShot>,
    spec: crate::widget::TerminalSpecimenSpec,
    scale: f32,
    at: (i32, i32),
}

/// The card the last capture's frame painted, and its raster's top-left in the window.
struct CapturedSceneCard {
    card: PaintedSceneCard,
    origin: (i32, i32),
}

thread_local! {
    static DECORATED_CARD: std::cell::RefCell<Option<std::rc::Rc<SceneCardShot>>> =
        const { std::cell::RefCell::new(None) };
    static PAINTED_CARD: std::cell::RefCell<Option<Box<PaintedSceneCard>>> =
        const { std::cell::RefCell::new(None) };
    static CAPTURED_CARD: std::cell::RefCell<Option<Box<CapturedSceneCard>>> =
        const { std::cell::RefCell::new(None) };
}

fn note_scene_card(shot: SceneCardShot) {
    DECORATED_CARD.set(Some(std::rc::Rc::new(shot)));
}

/// A raster blitted `spec`'s card at `at`, its device pixels: the card the scene last decorated
/// (a still scene's cached frame is the one it decorated) is the one a capture of it shows.
pub(crate) fn note_scene_card_painted(
    spec: &crate::widget::TerminalSpecimenSpec,
    scale: f32,
    at: (i32, i32),
) {
    if spec.motion.is_none() {
        return;
    }
    let Some(shot) = DECORATED_CARD.with_borrow(Clone::clone) else {
        return;
    };
    PAINTED_CARD.set(Some(Box::new(PaintedSceneCard {
        shot,
        spec: spec.clone(),
        scale,
        at,
    })));
}

/// A capture keeps the card its frame painted, the raster that holds it at `origin` in the
/// window's device pixels; `None` (a frame with no native page) keeps none.
pub(crate) fn stash_captured_scene_card(origin: Option<(i32, i32)>) {
    let card = origin.and_then(|origin| {
        PAINTED_CARD
            .with_borrow(|card| card.as_deref().cloned())
            .map(|card| Box::new(CapturedSceneCard { card, origin }))
    });
    CAPTURED_CARD.set(card);
}

/// `settings card`: the card the last capture's frame painted, as reply lines. `dims OK <rows>
/// <cols> <w> <h> cell_w=<px> cell_h=<px> pad=<x> pad_top=<y> head=0` (the card's grid in the
/// window, as the terminal's `dims` names a pane's), `cursor OK <row> <col> <visible> <style>`,
/// `status phase=<ms> pet_body=<x0>,<x1>,<y0>,<y1> word_icons=none` (window device pixels, end
/// exclusive, as `trail status` names the terminal companion's; `pet_body=none` with none drawn;
/// the card shows no word icon) and `text
/// <row>` for each row. With `clean`, one line `clean <x> <y> <w> <h> <bytes> <base64 PNG>`:
/// the same cells rendered with no light, no companion and no bloom (`image clean`'s twin), at
/// the card's place in the window.
pub(crate) fn captured_scene_card_reply(clean: bool) -> Result<Vec<String>, String> {
    CAPTURED_CARD.with_borrow(|captured| {
        let Some(CapturedSceneCard {
            card,
            origin: (ox, oy),
        }) = captured.as_deref()
        else {
            return Err(
                "settings card: the last capture painted no cursor scene card; \
                        capture a Cursor settings page with `image` first"
                    .to_string(),
            );
        };
        let (x, y) = (ox + card.at.0, oy + card.at.1);
        let shot = &card.shot;
        if clean {
            // The page under the card is no light: the wallpaper the card was laid on stays.
            let mut bare = shot.input.clone();
            let wallpaper = bare.wallpaper.take();
            bare.clear_overlays();
            bare.wallpaper = wallpaper;
            let frame = crate::tray_raster::render_bare_scene_card(&card.spec, bare, card.scale)
                .ok_or_else(|| "settings card clean: the bare card did not render".to_string())?;
            let rgba: Vec<u8> = frame
                .pixels
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            let (w, h) = (frame.width, frame.pixels.len() / frame.width.max(1));
            let png = crate::app_introspect::encode_rgba8_png(
                &rgba,
                u32::try_from(w).map_err(|e| e.to_string())?,
                u32::try_from(h).map_err(|e| e.to_string())?,
            )?;
            let encoded = aterm_codec::base64::encode(&png).map_err(|e| format!("{e:?}"))?;
            return Ok(vec![format!(
                "clean {x} {y} {w} {h} {} {encoded}",
                png.len()
            )]);
        }
        let input = &shot.input;
        let (cw, ch) = shot.cell;
        let mut lines = vec![
            format!(
                "dims OK {} {} {} {} cell_w={cw} cell_h={ch} pad={x} pad_top={y} head=0",
                input.rows,
                input.cols,
                input.cols * cw,
                input.rows * ch
            ),
            format!(
                "cursor OK {} {} {} {}",
                input.cursor_row,
                input.cursor_col,
                u8::from(input.cursor_visible),
                crate::control::cursor_style_name(input.cursor_style)
            ),
            format!(
                "status phase={} pet_body={} word_icons=none",
                shot.phase_ms,
                shot.body.map_or_else(
                    || "none".to_string(),
                    |(x0, x1, y0, y1)| format!("{},{},{},{}", x + x0, x + x1, y + y0, y + y1)
                )
            ),
        ];
        for row in input.cells.iter().take(input.rows) {
            let text: String = row
                .iter()
                .filter(|cell| !cell.wide)
                .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
                .collect();
            lines.push(format!("text {}", text.trim_end()));
        }
        Ok(lines)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneCompanion {
    None,
    Pet(aterm_effects::kitty_pet::PetSpecies),
    FlyingKitty,
}

/// The cursor scene's moving parts.
#[derive(Clone, Debug)]
pub(crate) struct CursorScene {
    cursor: CursorPreviewSpec,
    post_fx: CursorPostFxSpec,
    theme: Theme,
    phase_ms: u64,
    /// The scripted typing plays.
    typing: bool,
    /// The viewer paused the loop.
    held: bool,
    /// The blink phase (and the cursor style) show the caret.
    caret_shown: bool,
    companion: SceneCompanion,
    /// The Flying kitty is the selected style's, so the real cursor cat
    /// (`aterm_effects::kitty_cursor::CursorCat`) earns it as the terminal
    /// does; the sprite control's diagnostic kitty shows at once.
    flying_earned: bool,
    /// Tests' negative controls ([`SceneControls`]).
    #[cfg(test)]
    controls: SceneControls,
    /// The whole rows the card shows (`None`: every row of the grid).
    card_rows: Option<usize>,
    /// A wallpaper lies under the card (§15.2 1.7's page law, [`aterm_effects::host::PageFacts`]
    /// as a window reads its own). The card draws no translucency, so with no wallpaper its page
    /// is clear.
    wallpaper: bool,
    /// Total over every field above but the phase.
    script_key: u64,
    /// Total over every field above, for the raster frame memo.
    pub(crate) fingerprint: u64,
}

#[derive(Clone, Debug, Default)]
struct SceneStep {
    glow_add: Vec<GlowQuad>,
    glow_under: Vec<GlowQuad>,
    glow_halo: Vec<RainHalo>,
    fire_patch: Vec<aterm_core::render::FirePatch>,
    char_fg: Vec<aterm_core::render::CharFg>,
    fire_halo: Vec<aterm_core::render::FireHaloCell>,
    trail: Vec<aterm_core::render::TrailCell>,
    frame: Option<aterm_effects::cursor_fx::CursorFxFrame>,
    cat: Option<aterm_effects::kitty_cursor::CatFrame>,
    /// The theme's text holds the bloom, or its post passes spare the
    /// letters, as the live terminal's do.
    bloom_gate: aterm_core::render::BloomGate,
    #[cfg(test)]
    without_body_under: Option<Vec<GlowQuad>>,
}

struct ScenePlayer {
    key: u64,
    origin: Instant,
    fx: CursorFx,
    cat: aterm_effects::kitty_cursor::CursorCat,
    /// Scene time of the last step played, ms since `origin`.
    at: Option<u64>,
    step: SceneStep,
    /// The cells the step presents, handed to the trail as its frame's text.
    text: Vec<Vec<aterm_core::terminal::RenderCell>>,
}

impl ScenePlayer {
    fn new(key: u64, look: aterm_effects::kitty_registry::KittyLook) -> Self {
        let mut cat = aterm_effects::kitty_cursor::CursorCat::default();
        cat.set_look(look);
        Self {
            key,
            origin: Instant::now(),
            fx: CursorFx::default(),
            cat,
            at: None,
            step: SceneStep::default(),
            text: Vec::new(),
        }
    }
}

struct SceneStage<'a, 'renderer> {
    /// The specimen as the first line types on it.
    base: &'a RenderInput,
    /// The specimen once a Return has handed a line to the shell.
    scrolled: &'a RenderInput,
    row: usize,
    line: TypedLine,
    geom: Geom,
    cursor_style: aterm_core::terminal::CursorStyle,
    /// What the card's face lays down of each glyph ([`aterm_render::Renderer::glyph_ink`]),
    /// handed to the trail as the terminal's host hands it the live face's: its letter guard
    /// reads each letter's ink as the raster lays it. `None`: each glyph's ink as its colour.
    ink: Option<&'a std::sync::Arc<aterm_render::GlyphInk>>,
    renderer: Option<&'a std::cell::RefCell<&'renderer mut aterm_render::Renderer>>,
    #[cfg(test)]
    without_body_under: std::cell::RefCell<Option<Vec<GlowQuad>>>,
}

impl CursorScene {
    fn new(spec: &SettingsPreviewSpec, theme: Theme, card_rows: Option<usize>) -> Self {
        use std::hash::{Hash, Hasher};

        let sprite_diagnostic = spec.focused_key == crate::prefs::EDIT_CURSOR_NYAN_SPRITE;
        let held = spec.holds_pause();
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
                PreviewTrailCompanion::Pet(species) => SceneCompanion::Pet(species),
                PreviewTrailCompanion::FlyingKitty if !spec.reduced_motion => {
                    SceneCompanion::FlyingKitty
                }
                PreviewTrailCompanion::FlyingKitty | PreviewTrailCompanion::None => {
                    SceneCompanion::None
                }
            }
        };
        let hash_theme = |hash: &mut aterm_hash::FxHasher| {
            theme.fg.hash(hash);
            theme.bg.hash(hash);
            theme.cursor.hash(hash);
            theme.selection.hash(hash);
        };
        let mut script = aterm_hash::FxHasher::default();
        spec.clone()
            .with_phase(0)
            .paint_fingerprint()
            .hash(&mut script);
        hash_theme(&mut script);
        let mut hash = aterm_hash::FxHasher::default();
        spec.paint_fingerprint().hash(&mut hash);
        hash_theme(&mut hash);
        card_rows.hash(&mut hash);
        #[cfg(test)]
        let controls = SCENE_CONTROLS.with(std::cell::Cell::get);
        #[cfg(test)]
        {
            controls.hash(&mut script);
            controls.hash(&mut hash);
        }
        Self {
            cursor: spec.cursor.clone(),
            post_fx: spec.post_fx.clone(),
            theme,
            phase_ms: if held {
                CURSOR_SCENE_PAUSE_MS
            } else {
                spec.phase_ms % CURSOR_SCENE_CYCLE_MS
            },
            typing: held || spec.animation() == PreviewAnimation::Continuous,
            held,
            caret_shown: spec.cursor_visible(),
            companion,
            flying_earned: companion == SceneCompanion::FlyingKitty && !sprite_diagnostic,
            #[cfg(test)]
            controls,
            card_rows,
            wallpaper: spec.wallpaper.is_some(),
            script_key: script.finish(),
            fingerprint: hash.finish() | 1,
        }
    }

    /// The scripted typing plays, so every frame differs.
    pub(crate) const fn is_moving(&self) -> bool {
        self.typing && !self.held
    }

    const fn warm_cycles(&self) -> u64 {
        if self.flying_earned {
            1
        } else {
            CURSOR_SCENE_WARM_CYCLES
        }
    }

    const fn shown_offset(&self) -> u64 {
        self.phase_ms - self.phase_ms % MOTION_STEP_MS
    }

    /// Play the scene onto `base` for a renderer whose cells are `cell` device pixels.
    #[cfg(test)]
    pub(crate) fn decorate(&self, base: &RenderInput, cell: (usize, usize)) -> RenderInput {
        self.decorate_inked(base, cell, None)
    }

    /// [`Self::decorate`], the card's face laying each glyph as `ink` says
    /// ([`SceneStage::ink`]).
    #[cfg(test)]
    pub(crate) fn decorate_inked(
        &self,
        base: &RenderInput,
        cell: (usize, usize),
        ink: Option<&std::sync::Arc<aterm_render::GlyphInk>>,
    ) -> RenderInput {
        self.decorate_observed(base, cell, ink, None)
    }

    /// Play with actual current-frame glyph evidence from the same renderer that draws it.
    pub(crate) fn decorate_with_renderer(
        &self,
        base: &RenderInput,
        renderer: &mut aterm_render::Renderer,
    ) -> RenderInput {
        let cell = renderer.cell_size();
        let ink = renderer.glyph_ink();
        let source = std::cell::RefCell::new(renderer);
        self.decorate_observed(base, cell, Some(&ink), Some(&source))
    }

    fn decorate_observed(
        &self,
        base: &RenderInput,
        cell: (usize, usize),
        ink: Option<&std::sync::Arc<aterm_render::GlyphInk>>,
        renderer: Option<&std::cell::RefCell<&mut aterm_render::Renderer>>,
    ) -> RenderInput {
        #[cfg(test)]
        let ink = ink.filter(|_| self.controls.glyph_ink);
        let mut input = base.clone();
        let (cell_w, cell_h) = cell;
        let (rows, cols) = (input.rows, input.cols);
        if rows == 0 || cols == 0 || cell_w == 0 || cell_h == 0 || input.cells.len() < rows {
            return input;
        }
        let row = prompt_row(&input);
        let line = TypedLine::of(&input.cells[row]);
        let scrolled = self.typing.then(|| scroll_submitted_line(base, row));
        if let Some(scrolled) = &scrolled {
            input.cells.clone_from(&scrolled.cells);
        }
        let offset = self.shown_offset();
        let shown = if self.typing {
            line.shown_at(offset)
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
            .skip(line.caret(shown))
        {
            *cell = blank;
        }
        let caret = (row as u16, line.caret(shown).min(cols - 1) as u16);
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
        let mut cat = None;
        #[cfg(test)]
        let mut without_body_under = None;
        if self.typing {
            let stage = SceneStage {
                base,
                scrolled: scrolled.as_ref().unwrap_or(base),
                row,
                line,
                geom,
                cursor_style: input.cursor_style,
                ink,
                renderer,
                #[cfg(test)]
                without_body_under: std::cell::RefCell::new(None),
            };
            cat = self.play(&mut input, &stage, cell);
            #[cfg(test)]
            {
                without_body_under = stage.without_body_under.into_inner();
            }
        }
        let source = scrolled.as_ref().unwrap_or(base);
        let caret_at = |at: u64| line.caret(line.shown_at(at)).min(cols - 1);
        // Where the Return and each key of the loop left the caret, in order.
        let carets = if self.typing {
            (0..=CURSOR_SCENE_KEYS)
                .map(|key| caret_at(key * KEY_MS))
                .collect()
        } else {
            Vec::new()
        };
        let pet = PetCue {
            letters: SceneLetters {
                cells: &source.cells,
                row,
                line,
                caret: usize::from(caret.1),
            },
            carets,
            keys: usize::try_from((offset / KEY_MS).min(CURSOR_SCENE_KEYS)).unwrap_or(0),
            offset,
            typed: shown,
        };
        let body = self.place_companion(&mut input, cell, caret, &pet, cat);
        note_scene_card(SceneCardShot {
            phase_ms: self.phase_ms,
            cell,
            input: input.clone(),
            body,
            #[cfg(test)]
            without_body_under,
            #[cfg(test)]
            scene_fingerprint: self.fingerprint,
        });
        input
    }

    fn play(
        &self,
        input: &mut RenderInput,
        stage: &SceneStage<'_, '_>,
        cell: (usize, usize),
    ) -> Option<aterm_effects::kitty_cursor::CatFrame> {
        use std::hash::{Hash, Hasher};

        use aterm_effects::cursor_fx::compose_caret_style_override;

        thread_local! {
            // Boxed: a thread-local's value is part of the image's TLS template, which every
            // thread allocates whole on its first thread-local (ScenePlayer inline was 27 KB of
            // each of aterm's threads, docs/measured/idle-memory-2026-10-27.md).
            static PLAYER: std::cell::RefCell<Option<Box<ScenePlayer>>> =
                const { std::cell::RefCell::new(None) };
        }
        let mut key = aterm_hash::FxHasher::default();
        self.script_key.hash(&mut key);
        cell.hash(&mut key);
        (stage.row, stage.line.start, stage.line.end).hash(&mut key);
        (stage.geom.rows, stage.geom.cols).hash(&mut key);
        stage.ink.map(|ink| ink.fingerprint()).hash(&mut key);
        if let Some(renderer) = stage.renderer {
            let epoch = renderer.borrow().glyph_ink_epoch();
            epoch.hash(&mut key);
        }
        let key = key.finish();
        let target = self.warm_cycles() * CURSOR_SCENE_CYCLE_MS + self.shown_offset();
        let step = PLAYER.with_borrow_mut(|slot| {
            let player = match slot {
                Some(player) if player.key == key && player.at.is_some_and(|at| at <= target) => {
                    player
                }
                _ => {
                    let player =
                        slot.insert(Box::new(ScenePlayer::new(key, self.post_fx.companion_look)));
                    player.fx.glow.observe_glyph_ink(stage.ink.cloned());
                    player
                }
            };
            let mut next = player.at.map_or(0, |at| at + MOTION_STEP_MS);
            while next <= target {
                self.step(player, stage, next, next == target);
                player.at = Some(next);
                next += MOTION_STEP_MS;
            }
            player.step.clone()
        });
        #[cfg(test)]
        stage
            .without_body_under
            .replace(step.without_body_under.clone());
        input.cursor_glow_add = step.glow_add;
        input.glow_under = step.glow_under;
        input.glow_halo = step.glow_halo;
        input.fire_patch = step.fire_patch;
        input.char_fg = step.char_fg;
        input.fire_halo = step.fire_halo;
        input.cursor_trail = step.trail;
        input.bloom_gate = step.bloom_gate;
        if let Some(frame) = step.frame {
            input.cursor_trail_color = frame.trail_color;
            input.cursor_fill_override = frame.caret_fill();
            #[cfg(test)]
            if !self.controls.thin_caret_fill {
                input.cursor_fill_override = frame.block_fill.map(|owned| owned.fill);
            }
            input.cursor_effect_style_override = compose_caret_style_override(
                frame.bolt_cursor,
                frame.twinkle_cursor,
                frame.momentum_steady,
            );
        }
        if input
            .cursor_effect_style_override
            .is_some_and(|style| !style.blinks())
        {
            input.cursor_visible = true;
        }
        step.cat
    }

    fn step(&self, player: &mut ScenePlayer, stage: &SceneStage<'_, '_>, at: u64, keep: bool) {
        use aterm_effects::cursor_fx::head_dx_for;

        let ScenePlayer {
            origin,
            fx,
            cat: cursor_cat,
            step: kept,
            text,
            ..
        } = player;
        let now = *origin + Duration::from_millis(at);
        let offset = at % CURSOR_SCENE_CYCLE_MS;
        fx.begin(true, true);
        match SceneEvent::at(offset) {
            Some(SceneEvent::Return) if at > 0 => {
                fx.cadence.on_keystroke(now);
                fx.glow.supersede_typed_press();
                fx.glow.note_return(now);
                fx.trail.note_return(now);
                cursor_cat.on_enter(now);
                fx.scroll_translate(CURSOR_SCENE_RETURN_ROWS);
            }
            Some(SceneEvent::Glyph(cell)) => {
                let ch = CURSOR_SCENE_COMMAND[cell..].chars().next().unwrap_or(' ');
                fx.cadence.on_keystroke(now);
                fx.glow.supersede_typed_press();
                fx.momentum
                    .on_key(now, aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S);
                fx.glow.note_typed_expected(
                    now,
                    1,
                    ch.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch),
                    crate::app_input::typed_class_for(Some(ch)),
                    ch,
                );
                fx.trail.note_typed(now);
            }
            Some(SceneEvent::Tab) => {
                fx.cadence.on_keystroke(now);
                fx.glow.note_user_gesture(now);
                fx.trail.note_user_gesture(now);
                if self.cursor.trail_style == PreviewTrailStyle::RainbowKitty {
                    fx.glow
                        .note_insert_armed(now, aterm_effects::cursor_glow::InsertWidth::Unknown);
                }
            }
            Some(SceneEvent::Return) | None => {}
        }
        let (row, line) = (stage.row, stage.line);
        let base = if at >= CURSOR_SCENE_CYCLE_MS {
            stage.scrolled
        } else {
            stage.base
        };
        let shown = line.shown_at(offset);
        let caret = line.caret(shown).min(stage.geom.cols - 1) as u16;
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
        let below = base
            .cells
            .get(row + 1)
            .map(|cells| probe_of(cells, usize::MAX));
        fx.glow.observe_row_with_trust(
            row as u16,
            caret,
            &probe_of(&base.cells[row], line.caret(shown)),
            now,
            crate::cursor_glow::ProbeTrust::Full,
        );
        fx.glow
            .observe_neighbor_rows(above.as_deref(), below.as_deref());
        fx.glow.observe_caret_drawn(true);
        // The frame's text, as the terminal's host hands the trail the cells
        // it presents (`pipeline_native::step_cursor_family`): the letter
        // guard, the dust's air and the caret's letter read it.
        text.clone_from(&base.cells);
        if let Some(cells) = text.get_mut(row) {
            let blank = aterm_core::terminal::RenderCell {
                fg: crate::settings::u32_rgb(self.theme.fg),
                bg: crate::settings::u32_rgb(self.theme.bg),
                ..aterm_core::terminal::RenderCell::default()
            };
            for cell in cells.iter_mut().take(line.end).skip(line.caret(shown)) {
                *cell = blank;
            }
        }
        #[cfg(test)]
        if !self.controls.frame_text {
            text.clear();
        }
        #[cfg(test)]
        fx.glow.set_motes_cut(!self.controls.motes_whole);
        #[cfg(test)]
        fx.glow.set_rise_cut(!self.controls.rises_kept);
        #[cfg(test)]
        fx.glow
            .set_typed_life_scaled(self.controls.typed_life_scaled);
        #[cfg(test)]
        fx.glow.set_letter_guard_whole(self.controls.letter_whole);
        #[cfg(test)]
        fx.glow.set_letter_guard_shades(self.controls.shades_held);
        #[cfg(test)]
        let unseen = !keep && self.controls.unseen_steps_unguarded;
        #[cfg(not(test))]
        let unseen = !keep;
        fx.glow.set_frame_unseen(unseen);
        fx.glow.observe_frame_text_at(
            Some((row as u16, caret)),
            text,
            ((0, 0), stage.geom.cols),
            now,
        );
        // The card keeps a window's page law (`docs/design/PRISM-GLASS.md` §15.2 1.7, §15.8
        // amendments 53 and 54): its page takes the glass when it is clear and the card's face,
        // at the card's device size, lays its thinnest glyph's darkest pixel no further toward
        // the page than `colour::THIN_INK_SHARE`, read off the face's ink as `App::page_facts`
        // reads a window's. With no ink to read, it takes none.
        let lum = |c: u32| aterm_render::linear_luminance(c & 0x00FF_FFFF);
        let page = aterm_effects::host::PageFacts {
            opaque: true,
            wallpaper: self.wallpaper,
            thin_ink: stage.ink.map_or(1.0, |ink| {
                ink.thin_share(lum(self.theme.fg), lum(self.theme.bg))
            }),
        };
        let head_dx = head_dx_for(stage.cursor_style);
        let mut glow = self.cursor.glow_config(self.theme, head_dx, page.glass());
        let presentation = self.cursor.resolved_trail_presentation();
        let mut comet = TrailConfig {
            enabled: self.cursor.trail_enabled && presentation.comet,
            duration: Duration::from_millis(self.cursor.duration_ms),
            max_len: self.cursor.length,
            color: self
                .cursor
                .color
                .knob(self.theme.cursor)
                .unwrap_or(crate::cursor_glow::COMET_DEFAULT_COLOR),
            intensity: 0.0,
            warmth: 0.0,
        };
        aterm_effects::cursor_fx::follow_live_cursor(
            &mut glow,
            &mut comet,
            self.theme.cursor,
            self.cursor.cursor_color_pinned,
            self.cursor.color,
            self.cursor.accent.is_some(),
            None,
        );
        let frame = fx.tick(
            &CursorFxInput {
                now,
                cur: Some((row as u16, caret)),
                live_viewport: true,
                cursor_style: stage.cursor_style,
                blink_phase: true,
                live_cursor: self.theme.cursor,
                cursor_color_pinned: self.cursor.cursor_color_pinned,
                default_bg: self.theme.bg,
                cursor_cell_bg: None,
                geom: stage.geom,
                focused: true,
                shed_envelope: 1.0,
                trail_brightness: glow.intensity,
                body_allowed: true,
                momentum_glow: self.cursor.trail_enabled && self.cursor.momentum_glow,
                user_tinted: self.cursor.color.chosen(),
                accent_pinned: self.cursor.accent.is_some(),
            },
            &glow,
            comet,
        );
        let cat = self.flying_earned.then(|| {
            crate::app_render::forward_kitty_cursor_motion(
                self.cursor.trail_enabled,
                false,
                true,
                glow.style,
                fx.glow.take_cursor_cat_motion_pulse(),
                cursor_cat,
            );
            cursor_cat.frame(now)
        });
        if keep {
            // The retained step publishes this text, not the outer specimen before its
            // typing replay. Observe those exact glyphs and protect them through the same
            // yield as a terminal window before flattening its ramps for the card.
            let mut protected = base.clone();
            protected.cells.clone_from(text);
            protected.cursor_row = row;
            protected.cursor_col = usize::from(caret);
            protected.cursor_visible = self.caret_shown;
            protected.glow_under = fx.glow.under_quads().to_vec();
            protected.glow_under_ramps = fx.glow.under_ramps().clone();
            let observed = stage
                .ink
                .and(stage.renderer)
                .map(|renderer| renderer.borrow_mut().glyph_ink_for(&protected));
            fx.glow
                .observe_glyph_ink(observed.or_else(|| stage.ink.cloned()));
            #[cfg(test)]
            let without_body = self.controls.observe_body.then(|| {
                aterm_effects::cursor_glow::glass_yield::without_body_glass_for_test(
                    &protected,
                    fx.glow.glass_laid(),
                )
            });
            fx.glow.yield_glass(&mut protected, &[], now);
            // The scene keeps its glow as quads (its post passes read them),
            // every lit ramp row the quad it stands for.
            let quads = |quads: &[GlowQuad], ramps: &aterm_render::GlowRamps| {
                let mut all = Vec::new();
                ramps.expand_into(quads, &mut all);
                all
            };
            *kept = SceneStep {
                glow_add: quads(&fx.glow_scratch, fx.glow.out_ramps()),
                glow_under: quads(&protected.glow_under, &protected.glow_under_ramps),
                glow_halo: fx.glow.halos().to_vec(),
                fire_patch: fx.glow.patches().to_vec(),
                char_fg: fx.glow.charred().to_vec(),
                fire_halo: fx.glow.halo_cells().to_vec(),
                trail: fx.trail_scratch.clone(),
                frame: Some(frame),
                cat,
                bloom_gate: fx.bloom_gate(),
                #[cfg(test)]
                without_body_under: without_body
                    .map(|input| quads(&input.glow_under, &input.glow_under_ramps)),
            };
        }
    }

    /// The live terminal's post passes over the rendered scene `frame`, whose
    /// grid lies at `grid` (a frame whose bloom spares its letters crowns and
    /// blooms from the lights kept off them, as the live terminal's GPU does).
    pub(crate) fn post_process(
        &self,
        frame: &mut aterm_render::Frame,
        input: &RenderInput,
        grid: aterm_render::bloom::LetterGrid,
    ) -> Result<(), String> {
        if !self.typing {
            return Ok(());
        }
        let radius =
            finite_or(self.post_fx.bloom_radius, aterm_render::bloom::BLOOM_RADIUS).max(0.0);
        let (mut crown, mut blur) = (Vec::new(), Vec::new());
        let mut mask = None;
        let (crown, blur) = if input.bloom_gate.spares_letters() {
            mask = aterm_render::bloom::spared_sources(
                &input.cursor_glow_add,
                input,
                grid,
                radius,
                &mut crown,
                &mut blur,
            );
            (&crown[..], &blur[..])
        } else {
            (&input.cursor_glow_add[..], &input.cursor_glow_add[..])
        };
        if self.post_fx.bloom && !input.bloom_gate.held() {
            aterm_render::bloom::apply_kept_off(
                frame,
                blur,
                input.rows,
                aterm_render::bloom::BloomParams {
                    strength: finite_or(self.post_fx.bloom_strength, 0.0).max(0.0),
                    radius,
                },
                mask.as_ref(),
            )?;
        }
        let budget = if self.post_fx.hdr_glow && self.post_fx.display_edr {
            aterm_render::hdr::HDR_GLOW_BOOST
        } else {
            let luma = aterm_render::hdr::packed_luma(self.theme.bg);
            aterm_render::hdr::sdr_glow_budget(luma, self.post_fx.sdr_boost)
        };
        aterm_render::bloom::sdr_crown(frame, crown, input.rows, budget)
    }

    fn place_companion(
        &self,
        input: &mut RenderInput,
        (cell_w, cell_h): (usize, usize),
        caret: (u16, u16),
        pet: &PetCue<'_>,
        cat: Option<aterm_effects::kitty_cursor::CatFrame>,
    ) -> Option<(i32, i32, i32, i32)> {
        use aterm_effects::word_decorations::{EffectGeom, KittySpriteSource, WordDecorations};
        use aterm_render::sprites::{FreeSprite, SpriteLayers, WORD_SPRITE_SALT};

        if self.companion == SceneCompanion::None {
            return None;
        }
        let flight = match cat {
            Some(frame) if self.flying_earned => {
                if frame.alpha == 0 {
                    return None;
                }
                Some(frame)
            }
            _ => None,
        };
        let asset = &self.post_fx.kitty_asset;
        if self.companion == SceneCompanion::FlyingKitty
            && matches!(asset, crate::app_config::KittySpriteAsset::Invalid { .. })
        {
            return None;
        }
        let (Ok(cell_w), Ok(cell_h), Ok(rows), Ok(cols)) = (
            u16::try_from(cell_w),
            u16::try_from(cell_h),
            u16::try_from(input.rows),
            u16::try_from(input.cols),
        ) else {
            return None;
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
            // The companion's decorations and its layer publisher, so a still
            // preview frame keeps its sprite layers (identity damage repaints
            // nothing for them). Boxed, as `PLAYER` is: inline it was 3.6 KB of
            // every thread's TLS.
            static COMPANIONS: std::cell::RefCell<Option<Box<(u64, WordDecorations, SpriteLayers)>>> =
                const { std::cell::RefCell::new(None) };
        }
        COMPANIONS.with_borrow_mut(|slot| {
            let key = asset.fingerprint();
            if slot.as_deref().is_none_or(|(held, ..)| *held != key) {
                let mut decorations = WordDecorations::default();
                if let crate::app_config::KittySpriteAsset::Ready { w, h, rgba, fp, .. } = asset {
                    decorations.set_kitty_sprite_source(KittySpriteSource::Custom {
                        source_fp: *fp,
                        w: *w,
                        h: *h,
                        rgba: Arc::clone(rgba),
                    });
                }
                *slot = Some(Box::new((
                    key,
                    decorations,
                    SpriteLayers::new(WORD_SPRITE_SALT),
                )));
            }
            let (_, decorations, layers) = slot.as_deref_mut()?;
            let mut sprites = Vec::new();
            let shown_rows = self.card_rows.map_or(rows, |card| {
                u16::try_from(card).unwrap_or(u16::MAX).min(rows)
            });
            let floor = i32::from(shown_rows.max(1)) * i32::from(cell_h);
            let ink = (geom, colors);
            let mut emit = |fit: f32, sprites: &mut Vec<FreeSprite>| {
                for _ in 0..3 {
                    decorations.begin_host_frame();
                    self.emit_companion(
                        decorations,
                        ink,
                        caret,
                        (pet, floor),
                        flight,
                        fit,
                        sprites,
                    );
                    if !sprites.is_empty() {
                        break;
                    }
                }
            };
            emit(1.0, &mut sprites);
            let tallest = sprites.iter().map(|sprite| i32::from(sprite.h)).max();
            if let Some(tallest) = tallest.filter(|tallest| *tallest > floor) {
                let mut fitted = Vec::new();
                emit(floor as f32 / tallest as f32, &mut fitted);
                if !fitted.is_empty() {
                    sprites = fitted;
                }
            }
            for sprite in &mut sprites {
                sprite.y = sprite.y.min(floor - i32::from(sprite.h)).max(0);
            }
            layers.publish(&sprites, &mut input.fx_layers);
            sprites
                .iter()
                .map(|s| (s.x, s.x + i32::from(s.w), s.y, s.y + i32::from(s.h)))
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3)))
        })
    }

    #[allow(clippy::too_many_arguments)] // One companion frame's placement facts.
    fn emit_companion(
        &self,
        decorations: &mut aterm_effects::word_decorations::WordDecorations,
        (geom, colors): (
            aterm_effects::word_decorations::EffectGeom,
            aterm_effects::cat_baker::CatColorKey,
        ),
        caret: (u16, u16),
        (pet, floor): (&PetCue<'_>, i32),
        flight: Option<aterm_effects::kitty_cursor::CatFrame>,
        fit: f32,
        sprites: &mut Vec<aterm_render::sprites::FreeSprite>,
    ) {
        use aterm_effects::kitty_pet::PetAction;
        use aterm_effects::pet_glyphs_gen::PetGlyphId;
        use aterm_effects::word_decorations::{KittyCursorFrame, PetCursorFrame};

        let (cell_w, cell_h) = (geom.cell_w, geom.cell_h);
        match self.companion {
            SceneCompanion::Pet(species) => {
                let width = aterm_effects::kitty_pet::art_cols(cell_w, cell_h);
                let seat_for = |caret_col: usize, kept: Option<(f32, f32)>| {
                    let letters = SceneLetters {
                        caret: caret_col,
                        ..pet.letters
                    };
                    let caret = (caret.0, u16::try_from(caret_col).unwrap_or(u16::MAX));
                    #[cfg(test)]
                    if !self.controls.pet_seat_law {
                        return (
                            aterm_effects::kitty_pet::PetBrain::station(
                                caret.1,
                                geom.cols,
                                width * fit,
                            ),
                            f32::from(caret.0),
                        );
                    }
                    #[cfg(test)]
                    let kept = kept.filter(|_| self.controls.one_seat_a_line);
                    scene_pet_seat(letters, caret, geom, (width, fit), (floor, kept))
                };
                // The seat after the loop's Return and after each key since,
                // as the terminal's pet keeps one for the whole line: the
                // Return's own is the head law's afresh, as a submitted line's
                // is (`kitty_pet`'s ONE SEAT FOR THE WHOLE LINE holds only
                // while the hand writes).
                let mut seats: Vec<(f32, f32)> = Vec::with_capacity(pet.carets.len());
                for (i, &caret_col) in pet.carets.iter().enumerate() {
                    let kept = i.checked_sub(1).map(|before| seats[before]);
                    seats.push(seat_for(caret_col, kept));
                }
                let seat = seats
                    .get(pet.keys)
                    .copied()
                    .unwrap_or_else(|| seat_for(pet.letters.caret, None));
                // The newest key or Return that moved the seat, how long ago
                // and which way: a pet waiting on its seat while the caret
                // types towards it stands, then sits, rather than walk on the
                // spot. Before the Return it sat where the last loop's line
                // left it.
                let moved = (0..=pet.keys.min(seats.len().saturating_sub(1)))
                    .rev()
                    .find_map(|i| {
                        let before = match i {
                            0 => *seats.last()?,
                            i => seats[i - 1],
                        };
                        let ago = pet.offset.saturating_sub(i as u64 * KEY_MS);
                        (seats[i] != before).then_some((ago, seats[i].0 < before.0))
                    });
                let (action, pose) = match moved {
                    Some((ago, _)) if ago < KEY_MS * 2 => {
                        let walk = [
                            PetGlyphId::PetWalk0,
                            PetGlyphId::PetWalk1,
                            PetGlyphId::PetWalk2,
                            PetGlyphId::PetWalk3,
                        ];
                        (PetAction::Walk, walk[pet.typed % walk.len()])
                    }
                    Some((ago, _)) if ago < 500 => (PetAction::Stand, PetGlyphId::PetStand),
                    _ => (PetAction::Sit, PetGlyphId::PetSit),
                };
                let mut frame = scene_pet_frame(seat, (width, fit), (action, species.skin(pose)));
                frame.facing_left = moved.is_some_and(|(_, left)| left);
                let _ = decorations.pet_cursor(
                    PetCursorFrame {
                        geom,
                        colors,
                        coat: self.post_fx.companion_look.coat,
                        iris: self.post_fx.companion_look.iris,
                        pet: frame,
                        page: None,
                    },
                    sprites,
                );
            }
            SceneCompanion::FlyingKitty => {
                let (look, alpha, bob, mut pose, sing) = match flight {
                    Some(frame) => (
                        frame.render_look(),
                        frame.alpha,
                        frame.bob,
                        frame.pose,
                        frame.sing,
                    ),
                    None => (
                        self.post_fx.companion_look,
                        255,
                        0.0,
                        aterm_effects::kitty_cursor::CatPose::STILL,
                        0.0,
                    ),
                };
                // The terminal's seat for the flying head
                // (`EffectsPipeline::route_flying_head`): its rest ahead of the
                // caret where the caret's row is blank there, else behind the
                // caret or a row up, off the letters (`rainbow_kitty::companion::
                // placement`). The scene's cat faces the way it types.
                let mut lift = 0.0;
                if let Some(rest) = decorations.kitty_cursor_footprint(
                    aterm_effects::word_decorations::KittyCursorLayout {
                        geom,
                        cursor: caret,
                        look,
                        bob,
                        lift: 0.0,
                    },
                ) {
                    use aterm_effects::rainbow_kitty::companion::{SeatQuery, placement};

                    let ink = |row: u16, col: u16| {
                        (row < geom.rows && col < geom.cols)
                            .then(|| pet.letters.lettered(usize::from(row), usize::from(col)))
                    };
                    let seat = placement(
                        caret,
                        &SeatQuery {
                            geom: Geom {
                                cw: usize::from(cell_w),
                                ch: usize::from(cell_h),
                                rows: usize::from(geom.rows),
                                cols: usize::from(geom.cols),
                                origin_x: 0,
                                origin_y: 0,
                                win_w: 0,
                                win_h: 0,
                                head: 0,
                            },
                            rest,
                            lead_px: aterm_effects::word_decorations::WordDecorations::kitty_cursor_lead_px(cell_w),
                            held_sky: false,
                        },
                        ink,
                    );
                    lift = seat.lift(cell_h);
                    if cell_w > 0 {
                        pose.lead += seat.x.saturating_sub(rest.x) as f32 / f32::from(cell_w);
                    }
                }
                let _ = decorations.kitty_cursor(
                    KittyCursorFrame {
                        geom,
                        cursor: caret,
                        look,
                        colors,
                        bob,
                        lift,
                        alpha,
                        pose: aterm_effects::kitty_cursor::CatPose {
                            scale_x: pose.scale_x * fit,
                            scale_y: pose.scale_y * fit,
                            ..pose
                        },
                        sing,
                        notes: [None; aterm_effects::kitty_sing::MAX_NOTES],
                    },
                    sprites,
                );
            }
            SceneCompanion::None => {}
        }
    }
}

fn hash_trimmed_lowercase<H: std::hash::Hasher>(value: &str, hash: &mut H) {
    for byte in value.trim().bytes() {
        H::write_u8(hash, byte.to_ascii_lowercase());
    }
    H::write_u8(hash, 0xff);
}

fn header_state_x(rect: LogicalRect, inset: f32, control_reserve: f32, state_width: f32) -> f32 {
    (rect.right() - inset - control_reserve - state_width).max(rect.x + inset)
}

fn paint_pause_glyph(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    header_h: f32,
    paused: bool,
    color: [u8; 3],
) {
    let cy = y + header_h * 0.5 - 2.0;
    if paused {
        let (left, right) = (x + 3.0, x + PAUSE_GLYPH_W - 2.0);
        for (x1, y1, x2, y2) in [
            (left, cy - 5.0, left, cy + 5.0),
            (left, cy - 5.0, right, cy),
            (left, cy + 5.0, right, cy),
        ] {
            prims.push(DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width: 1.75,
                color: rgba(color, 235),
            });
        }
    } else {
        for bar_x in [x + 4.5, x + PAUSE_GLYPH_W - 4.5] {
            prims.push(DrawPrim::Line {
                x1: bar_x,
                y1: cy - 5.0,
                x2: bar_x,
                y2: cy + 5.0,
                width: 3.0,
                color: rgba(color, 220),
            });
        }
    }
}

const PREVIEW_NODE_LEAST: f32 = 96.0;

/// Build a Settings preview node.
pub(crate) fn preview_node(
    key: impl Into<std::sync::Arc<str>>,
    spec: SettingsPreviewSpec,
    height: f32,
) -> UiNode {
    UiNode::new(key, UiContent::SettingsPreview(Box::new(spec))).layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height.max(PREVIEW_NODE_LEAST)))
            .padding(Insets::default())
            .clipped(),
    )
}

fn paint_demo_dots_strip(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    layout: &DemoStripLayout,
    strip: &crate::tab_bar::StripSample,
    roles: Roles,
    caption: crate::type_scale::StepPx,
) {
    let top = rect.y;
    let strip_h = layout.strip_h;
    for index in 0..3 {
        prims.push(DrawPrim::Dot {
            cx: rect.x + 14.0 + index as f32 * 10.0,
            cy: top + strip_h * 0.5,
            r: 3.5,
            color: rgba(roles.text_tertiary, 255),
        });
    }
    let (tabs_x, tab_w, tab_top) = (layout.tabs_x, layout.tab_w, layout.tab_top);
    let tab_h = layout.tab_h(rect);
    let [selected, quiet] = layout.titles;
    if tab_w >= DemoStripLayout::MIN_TAB_W {
        prims.push(DrawPrim::Panel {
            x: tabs_x,
            y: tab_top,
            w: tab_w,
            h: tab_h,
            radius: 6.0,
            fill: rgba(strip.active, 255),
        });
        prims.push(DrawPrim::Panel {
            x: tabs_x,
            y: tab_top + tab_h * 0.5,
            w: tab_w,
            h: tab_h * 0.5,
            radius: 0.0,
            fill: rgba(strip.active, 255),
        });
        let baseline = crate::tray_raster::row_baseline(tab_top, tab_h - 2.0, caption.get());
        for (x, title, ink) in [
            (tabs_x + 10.0, selected, strip.active_ink),
            (tabs_x + tab_w + 11.0, quiet, strip.inactive_ink),
        ] {
            if let Some(title) = title {
                prims.push(text_prim(
                    x,
                    baseline,
                    title.to_string(),
                    caption,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(ink, 255),
                ));
            }
        }
        prims.push(DrawPrim::Panel {
            x: tabs_x + tab_w,
            y: tab_top + 4.0,
            w: 1.0,
            h: tab_h - 8.0,
            radius: 0.0,
            fill: rgba(strip.seam, 255),
        });
    }
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: top + strip_h - 1.0,
        w: rect.width,
        h: 1.0,
        radius: 0.0,
        fill: rgba(strip.seam, 255),
    });
    if tab_w >= DemoStripLayout::MIN_TAB_W && SettingsPreviewSpec::demo_grid_rect(rect).is_some() {
        prims.push(DrawPrim::Panel {
            x: tabs_x,
            y: top + strip_h - 2.0,
            w: tab_w,
            h: 2.0,
            radius: 0.0,
            fill: rgba(strip.rule, 255),
        });
    }
}

fn paint_demo_windows_strip(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    layout: &DemoStripLayout,
    strip: &crate::tab_bar::StripSample,
    ground: [u8; 3],
    fg: [u8; 3],
    caption: crate::type_scale::StepPx,
) {
    use aterm_messages::ink::ensure_contrast;
    let top = rect.y;
    let foot = top + layout.strip_h;
    let seam_y = foot - 1.0;
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: seam_y,
        w: rect.width,
        h: 1.0,
        radius: 0.0,
        fill: rgba(strip.seam, 255),
    });
    let px = caption.get();
    let cy = layout.label_cy(rect);
    let (tabs_x, tab_w, tab_top) = (layout.tabs_x, layout.tab_w, layout.tab_top);
    if tab_w >= DemoStripLayout::MIN_TAB_W {
        let opens = strip.active == ground;
        let bottom = if opens { foot } else { seam_y };
        prims.push(DrawPrim::ClipPush {
            x: tabs_x,
            y: top,
            w: tab_w,
            h: bottom - top,
        });
        prims.push(DrawPrim::Panel {
            x: tabs_x,
            y: tab_top,
            w: tab_w,
            h: bottom - tab_top + DEMO_TAB_RADIUS,
            radius: DEMO_TAB_RADIUS,
            fill: rgba(strip.active, 255),
        });
        if crate::tab_bar::windows_tab_wears_rim(strip.active, strip.band) {
            prims.push(DrawPrim::Stroke {
                x: tabs_x + 0.5,
                y: tab_top + 0.5,
                w: tab_w - 1.0,
                h: bottom - tab_top + DEMO_TAB_RADIUS,
                radius: DEMO_TAB_RADIUS - 0.5,
                width: 1.0,
                color: rgba(strip.seam, 255),
            });
        }
        prims.push(DrawPrim::ClipPop);
        let baseline = crate::tray_raster::row_baseline(tab_top, layout.tab_h(rect) - 2.0, px);
        let [selected, quiet] = layout.titles;
        let [selected_face, quiet_face] = DemoFrame::Windows.faces();
        let close_cx = tabs_x + tab_w - 14.0;
        for (x, title, face, ink) in [
            (tabs_x + 10.0, selected, selected_face, strip.active_ink),
            (tabs_x + tab_w + 10.0, quiet, quiet_face, strip.inactive_ink),
        ] {
            if let Some(title) = title {
                prims.push(text_prim(
                    x,
                    baseline,
                    title.to_string(),
                    caption,
                    TextWeight::Regular,
                    face,
                    rgba(ink, 255),
                ));
            }
        }
        let ink = rgba(strip.active_ink, 255);
        let (r, width) = ((px * 0.26).max(2.0), (px / 9.0).max(1.0));
        for dy in [-r, r] {
            prims.push(DrawPrim::Line {
                x1: close_cx - r,
                y1: cy - dy,
                x2: close_cx + r,
                y2: cy + dy,
                width,
                color: ink,
            });
        }
        if let Some(plus_cx) = layout.new_tab {
            let ink = rgba(ensure_contrast(fg, strip.band, 4.5), 255);
            let (r, width) = ((px * 0.30).max(2.5), (px / 8.5).max(1.2));
            prims.push(DrawPrim::Line {
                x1: plus_cx - r,
                y1: cy,
                x2: plus_cx + r,
                y2: cy,
                width,
                color: ink,
            });
            prims.push(DrawPrim::Line {
                x1: plus_cx,
                y1: cy - r,
                x2: plus_cx,
                y2: cy + r,
                width,
                color: ink,
            });
        }
    }
    if let Some(([minimize, maximize, close], glyph_cy)) = layout.caption {
        let ink = rgba(
            if crate::tab_bar::bg_is_light(strip.band) {
                [0, 0, 0]
            } else {
                [0xff, 0xff, 0xff]
            },
            255,
        );
        let half = DEMO_CAPTION_GLYPH * 0.5;
        prims.push(DrawPrim::Panel {
            x: minimize - half,
            y: glyph_cy,
            w: DEMO_CAPTION_GLYPH,
            h: 1.0,
            radius: 0.0,
            fill: ink,
        });
        prims.push(DrawPrim::Stroke {
            x: maximize - half + 0.5,
            y: glyph_cy - half + 0.5,
            w: DEMO_CAPTION_GLYPH - 1.0,
            h: DEMO_CAPTION_GLYPH - 1.0,
            radius: 1.0,
            width: 1.0,
            color: ink,
        });
        let arm = half - 0.5;
        for dy in [-arm, arm] {
            prims.push(DrawPrim::Line {
                x1: close - arm,
                y1: glyph_cy - dy,
                x2: close + arm,
                y2: glyph_cy + dy,
                width: 1.0,
                color: ink,
            });
        }
    }
}

fn push_demo_shadow(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    light_page: bool,
    ring: bool,
    body_radius: f32,
) {
    let shadow = if light_page { 0.18 } else { 0.30 };
    let body_radius = body_radius.max(0.0);
    for layer in 0..4 {
        let spread = 2.0 + layer as f32 * 3.0;
        let panel = DrawPrim::Panel {
            x: rect.x - spread,
            y: rect.y + 4.0 - spread * 0.5,
            w: rect.width + spread * 2.0,
            h: rect.height + spread,
            radius: body_radius + spread,
            fill: rgba([0, 0, 0], (shadow * 255.0 / 6.0) as u8),
        };
        if !ring || rect.width <= body_radius * 2.0 || rect.height <= body_radius * 2.0 {
            prims.push(panel);
            continue;
        }
        let (x0, y0) = (rect.x - spread - 1.0, rect.y + 4.0 - spread * 0.5 - 1.0);
        let (x1, y1) = (
            rect.right() + spread + 1.0,
            rect.bottom() + 4.0 + spread * 0.5 + 1.0,
        );
        let (inner_x0, inner_y0) = (rect.x + body_radius, rect.y + body_radius);
        let (inner_x1, inner_y1) = (rect.right() - body_radius, rect.bottom() - body_radius);
        for (x, y, right, bottom) in [
            (x0, y0, x1, inner_y0),
            (x0, inner_y1, x1, y1),
            (x0, inner_y0, inner_x0, inner_y1),
            (inner_x1, inner_y0, x1, inner_y1),
        ] {
            prims.push(DrawPrim::ClipPush {
                x,
                y,
                w: (right - x).max(0.0),
                h: (bottom - y).max(0.0),
            });
            prims.push(panel.clone());
            prims.push(DrawPrim::ClipPop);
        }
    }
}

const fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

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

fn friendly_preview_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Default".to_string();
    }
    let value = value.replace(['_', '-'], " ");
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

/// Settings ▸ Themes' chooser rows.
#[path = "settings_preview_theme_card.rs"]
pub(crate) mod theme_card;

#[cfg(test)]
#[path = "settings_preview_tests.rs"]
mod tests;
