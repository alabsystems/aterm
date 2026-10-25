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
            Self::Unfocused | Self::Load | Self::Resting | Self::Viewer => "Paused".to_string(),
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

/// The cursor companion a rainbow-family spelling selects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewTrailCompanion {
    #[default]
    None,
    Pet(aterm_effects::kitty_pet::PetSpecies),
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
            Self::RainbowKitty => "rainbow kitty",
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
        crate::app_config::resolve_trail_presentation_from_style(raw, self.resolved_trail_style())
    }

    /// The rainbow companion the runtime draws, read from the same resolved presentation.
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
            theme_bg_is_dark(theme.bg),
            theme.fg & 0x00FF_FFFF,
            theme.bg & 0x00FF_FFFF,
            head_dx,
        )
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
        trim_contrast(text),
        if illegible == 1 { "colour" } else { "colours" },
    )
}

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
            .map_or_else(
                || (self.font_px * 0.6, self.font_px * self.line_height * 1.25),
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
            && crate::prefs::theme_mode(Some(raw)) == crate::prefs::ThemeMode::FollowSystem
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

fn scroll_previous_line(base: &RenderInput, row: usize) -> RenderInput {
    let mut scrolled = base.clone();
    if row > 0 && row < scrolled.cells.len() {
        let previous = scrolled.cells[row].clone();
        scrolled.cells[..row].rotate_left(1);
        scrolled.cells[row - 1] = previous;
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
/// and the Return from the line's end down to the next prompt's first column.
pub(crate) const fn scene_jumps() -> [SceneJump; 2] {
    [
        SceneJump {
            rows: 0,
            cols: (CURSOR_SCENE_TAB.1 - CURSOR_SCENE_TAB.0) as u16,
            over_letters: true,
        },
        SceneJump {
            rows: 1,
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

fn scene_last_key_ms(offset: u64) -> u64 {
    (offset / KEY_MS).min(CURSOR_SCENE_KEYS) * KEY_MS
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
    /// The whole rows the card shows (`None`: every row of the grid).
    card_rows: Option<usize>,
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
    /// The theme's text holds the bloom, as the live terminal's does.
    bloom_held: bool,
}

struct ScenePlayer {
    key: u64,
    origin: Instant,
    fx: CursorFx,
    cat: aterm_effects::kitty_cursor::CursorCat,
    /// Scene time of the last step played, ms since `origin`.
    at: Option<u64>,
    step: SceneStep,
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
        }
    }
}

struct SceneStage<'a> {
    /// The specimen as the first line types on it.
    base: &'a RenderInput,
    /// The specimen once a Return has handed a line to the shell.
    scrolled: &'a RenderInput,
    row: usize,
    line: TypedLine,
    geom: Geom,
    cursor_style: aterm_core::terminal::CursorStyle,
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
            card_rows,
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
    pub(crate) fn decorate(&self, base: &RenderInput, cell: (usize, usize)) -> RenderInput {
        let mut input = base.clone();
        let (cell_w, cell_h) = cell;
        let (rows, cols) = (input.rows, input.cols);
        if rows == 0 || cols == 0 || cell_w == 0 || cell_h == 0 || input.cells.len() < rows {
            return input;
        }
        let row = prompt_row(&input);
        let line = TypedLine::of(&input.cells[row]);
        let scrolled = self.typing.then(|| scroll_previous_line(base, row));
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
        if self.typing {
            let stage = SceneStage {
                base,
                scrolled: scrolled.as_ref().unwrap_or(base),
                row,
                line,
                geom,
                cursor_style: input.cursor_style,
            };
            cat = self.play(&mut input, &stage, cell);
        }
        let settled_ms = if self.typing {
            offset - scene_last_key_ms(offset)
        } else {
            u64::MAX
        };
        self.place_companion(&mut input, cell, caret, shown, settled_ms, cat);
        input
    }

    fn play(
        &self,
        input: &mut RenderInput,
        stage: &SceneStage<'_>,
        cell: (usize, usize),
    ) -> Option<aterm_effects::kitty_cursor::CatFrame> {
        use std::hash::{Hash, Hasher};

        use aterm_effects::cursor_fx::compose_caret_style_override;

        thread_local! {
            static PLAYER: std::cell::RefCell<Option<ScenePlayer>> =
                const { std::cell::RefCell::new(None) };
        }
        let mut key = aterm_hash::FxHasher::default();
        self.script_key.hash(&mut key);
        cell.hash(&mut key);
        (stage.row, stage.line.start, stage.line.end).hash(&mut key);
        (stage.geom.rows, stage.geom.cols).hash(&mut key);
        let key = key.finish();
        let target = self.warm_cycles() * CURSOR_SCENE_CYCLE_MS + self.shown_offset();
        let step = PLAYER.with_borrow_mut(|slot| {
            let player = match slot {
                Some(player) if player.key == key && player.at.is_some_and(|at| at <= target) => {
                    player
                }
                _ => slot.insert(ScenePlayer::new(key, self.post_fx.companion_look)),
            };
            let mut next = player.at.map_or(0, |at| at + MOTION_STEP_MS);
            while next <= target {
                self.step(player, stage, next, next == target);
                player.at = Some(next);
                next += MOTION_STEP_MS;
            }
            player.step.clone()
        });
        input.cursor_glow_add = step.glow_add;
        input.glow_under = step.glow_under;
        input.glow_halo = step.glow_halo;
        input.fire_patch = step.fire_patch;
        input.char_fg = step.char_fg;
        input.fire_halo = step.fire_halo;
        input.cursor_trail = step.trail;
        input.bloom_gate = aterm_core::render::BloomGate::of(step.bloom_held);
        if let Some(frame) = step.frame {
            input.cursor_trail_color = frame.trail_color;
            input.cursor_fill_override = frame.block_fill.map(|owned| owned.fill);
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

    fn step(&self, player: &mut ScenePlayer, stage: &SceneStage<'_>, at: u64, keep: bool) {
        use aterm_effects::cursor_fx::head_dx_for;

        let ScenePlayer {
            origin,
            fx,
            cat: cursor_cat,
            step: kept,
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
                fx.scroll_translate(1);
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
        let mut glow = self
            .cursor
            .glow_config(self.theme, head_dx_for(stage.cursor_style));
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
                glow.style,
                fx.glow.take_cursor_cat_motion_pulse(),
                cursor_cat,
            );
            cursor_cat.frame(now)
        });
        if keep {
            *kept = SceneStep {
                glow_add: fx.glow_scratch.clone(),
                glow_under: fx.glow.under_quads().to_vec(),
                glow_halo: fx.glow.halos().to_vec(),
                fire_patch: fx.glow.patches().to_vec(),
                char_fg: fx.glow.charred().to_vec(),
                fire_halo: fx.glow.halo_cells().to_vec(),
                trail: fx.trail_scratch.clone(),
                frame: Some(frame),
                cat,
                bloom_held: fx.bloom_held,
            };
        }
    }

    /// The live terminal's post passes over the rendered scene `frame`.
    pub(crate) fn post_process(
        &self,
        frame: &mut aterm_render::Frame,
        input: &RenderInput,
    ) -> Result<(), String> {
        if !self.typing {
            return Ok(());
        }
        if self.post_fx.bloom && !input.bloom_gate.held() {
            aterm_render::bloom::apply(
                frame,
                &input.cursor_glow_add,
                input.rows,
                aterm_render::bloom::BloomParams {
                    strength: finite_or(self.post_fx.bloom_strength, 0.0).max(0.0),
                    radius: finite_or(self.post_fx.bloom_radius, aterm_render::bloom::BLOOM_RADIUS)
                        .max(0.0),
                },
            )?;
        }
        let budget = if self.post_fx.hdr_glow && self.post_fx.display_edr {
            aterm_render::hdr::HDR_GLOW_BOOST
        } else {
            let luma = aterm_render::hdr::packed_luma(self.theme.bg);
            aterm_render::hdr::sdr_glow_budget(luma, self.post_fx.sdr_boost)
        };
        aterm_render::bloom::sdr_crown(frame, &input.cursor_glow_add, input.rows, budget)
    }

    fn place_companion(
        &self,
        input: &mut RenderInput,
        (cell_w, cell_h): (usize, usize),
        caret: (u16, u16),
        typed: usize,
        settled_ms: u64,
        cat: Option<aterm_effects::kitty_cursor::CatFrame>,
    ) {
        use aterm_effects::word_decorations::{EffectGeom, KittySpriteSource, WordDecorations};
        use aterm_render::sprites::{FreeSprite, SpriteLayers, WORD_SPRITE_SALT};

        if self.companion == SceneCompanion::None {
            return;
        }
        let flight = match cat {
            Some(frame) if self.flying_earned => {
                if frame.alpha == 0 {
                    return;
                }
                Some(frame)
            }
            _ => None,
        };
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
            // The companion's decorations and its layer publisher, so a still
            // preview frame keeps its sprite layers (identity damage repaints
            // nothing for them).
            static COMPANIONS: std::cell::RefCell<Option<(u64, WordDecorations, SpriteLayers)>> =
                const { std::cell::RefCell::new(None) };
        }
        COMPANIONS.with_borrow_mut(|slot| {
            let key = asset.fingerprint();
            if slot.as_ref().is_none_or(|(held, ..)| *held != key) {
                let mut decorations = WordDecorations::default();
                if let crate::app_config::KittySpriteAsset::Ready { w, h, rgba, fp, .. } = asset {
                    decorations.set_kitty_sprite_source(KittySpriteSource::Custom {
                        source_fp: *fp,
                        w: *w,
                        h: *h,
                        rgba: Arc::clone(rgba),
                    });
                }
                *slot = Some((key, decorations, SpriteLayers::new(WORD_SPRITE_SALT)));
            }
            let Some((_, decorations, layers)) = slot.as_mut() else {
                return;
            };
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
                        typed,
                        settled_ms,
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
        });
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
        typed: usize,
        settled_ms: u64,
        flight: Option<aterm_effects::kitty_cursor::CatFrame>,
        fit: f32,
        sprites: &mut Vec<aterm_render::sprites::FreeSprite>,
    ) {
        use aterm_effects::kitty_pet::{
            PET_DEPARTURES_MAX, PET_MOTES_MAX, PetAction, PetBrain, PetFrame,
        };
        use aterm_effects::pet_glyphs_gen::PetGlyphId;
        use aterm_effects::word_decorations::{KittyCursorFrame, PetCursorFrame};

        let (cell_w, cell_h, cols) = (geom.cell_w, geom.cell_h, geom.cols);
        match self.companion {
            SceneCompanion::Pet(species) => {
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
                let station = PetBrain::station(caret.1, cols, width * fit);
                let col = station - width * (1.0 - fit) / 2.0;
                let _ = decorations.pet_cursor(
                    PetCursorFrame {
                        geom,
                        colors,
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
                        page: None,
                    },
                    sprites,
                );
            }
            SceneCompanion::FlyingKitty => {
                let (look, alpha, bob, pose, sing) = match flight {
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
                let _ = decorations.kitty_cursor(
                    KittyCursorFrame {
                        geom,
                        cursor: caret,
                        look,
                        colors,
                        bob,
                        lift: 0.0,
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
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::native_ui::{GroupSpec, SemanticRole, UiTree};

    /// The companions a preview frame draws: its sprite layers, read back.
    fn sprites(input: &RenderInput) -> Vec<aterm_render::sprites::FreeSprite> {
        aterm_render::sprites::published(&input.fx_layers)
    }

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

    fn played(spec: &SettingsPreviewSpec, theme: Theme) -> RenderInput {
        let spec = spec.normalized();
        let (base, _) = build_terminal_specimen_input(&spec, theme);
        CursorScene::new(&spec, theme, None).decorate(&base, (8, 17))
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

    fn compiled_with_font(
        mut spec: SettingsPreviewSpec,
        perturb: impl FnOnce(&mut crate::tray_raster::PreparedSemanticFont),
    ) -> crate::native_ui::CompiledUi {
        let _ = crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences::default(),
        );
        thread_local! {
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

    #[test]
    fn the_demo_shadow_ring_paints_what_the_full_shadow_shows() {
        let rect = LogicalRect::new(40.0, 30.0, 300.0, 180.0);
        for radius in [DemoFrame::Dots.radius(), DemoFrame::Windows.radius()] {
            let body = DrawPrim::Panel {
                x: rect.x,
                y: rect.y,
                w: rect.width,
                h: rect.height,
                radius,
                fill: rgba([30, 34, 40], 255),
            };
            let raster = |ring: bool, light: bool, with_body: bool, scale: f32| {
                let mut prims = Vec::new();
                push_demo_shadow(&mut prims, rect, light, ring, radius);
                if with_body {
                    prims.push(body.clone());
                }
                crate::tray_raster::rasterize_tray(&prims, 400, 260, scale, [236, 236, 236, 255]).0
            };
            for light in [true, false] {
                for scale in [1.0, 2.0] {
                    assert!(
                        raster(true, light, true, scale) == raster(false, light, true, scale),
                        "radius {radius}, light {light} at {scale}x"
                    );
                }
            }
            assert!(raster(true, true, false, 1.0) != raster(false, true, false, 1.0));
        }
    }

    fn pixels(spec: SettingsPreviewSpec) -> Vec<u8> {
        raster_of(&compiled(spec).tray(Theme::default(), 13.0).prims)
    }

    fn solid_wallpaper(rgb: [u8; 3], dim: f32, tint: bool) -> crate::widget::SpecimenWallpaper {
        crate::widget::SpecimenWallpaper {
            rgba: Arc::from(
                std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 8)
                    .flatten()
                    .collect::<Vec<u8>>(),
            ),
            w: 4,
            h: 2,
            fp: u64::from(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]])) ^ 0x5157,
            dim,
            tint,
        }
    }

    fn wallpaper_spec(wallpaper: Option<crate::widget::SpecimenWallpaper>) -> SettingsPreviewSpec {
        SettingsPreviewSpec {
            scene: PreviewScene::Wallpaper,
            wallpaper,
            terminal_theme: Some(PreviewTerminalTheme::from_scheme(
                &aterm_types::ColorScheme::default(),
            )),
            focused_key: crate::prefs::EDIT_WALLPAPER.to_string(),
            focused_value: "wall.png".to_string(),
            ..SettingsPreviewSpec::appearance(14.0)
        }
    }

    fn texel(raster: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * VIEWPORT.width as usize + x) * 4;
        [raster[i], raster[i + 1], raster[i + 2]]
    }

    #[test]
    fn the_wallpaper_preview_draws_the_picture_under_the_terminal() {
        let bg = crate::settings::u32_rgb(Theme::default().bg);
        let red = [0xE0, 0x20, 0x20];
        let (x, y) = (
            (VIEWPORT.width - 10.0 - 10.0 - 6.0) as usize,
            (10.0 + 190.0 - 10.0 - 6.0) as usize,
        );
        let at = |wallpaper| texel(&pixels(wallpaper_spec(wallpaper)), x, y);
        assert_eq!(at(None), bg, "no wallpaper: the theme background");
        assert_eq!(at(Some(solid_wallpaper(red, 0.0, false))), red, "dim 0");
        assert_eq!(at(Some(solid_wallpaper(red, 1.0, false))), bg, "dim 1");
        let mixed = at(Some(solid_wallpaper(red, 0.3, false)));
        for channel in 0..3 {
            let want = f32::from(red[channel]) * 0.7 + f32::from(bg[channel]) * 0.3;
            assert!(
                (f32::from(mixed[channel]) - want).abs() <= 2.0,
                "dim 0.3: {mixed:?} against {want} on channel {channel}"
            );
        }
        let tinted = |tint| pixels(wallpaper_spec(Some(solid_wallpaper(red, 0.0, tint))));
        assert_ne!(tinted(true), tinted(false), "the tint moves the text's ink");
        assert_ne!(
            wallpaper_spec(Some(solid_wallpaper(red, 0.3, false))).paint_fingerprint(),
            wallpaper_spec(Some(solid_wallpaper(red, 0.4, false))).paint_fingerprint()
        );
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
        differences_up_to(left, right, usize::MAX)
    }

    fn differences_up_to(left: &[u8], right: &[u8], cap: usize) -> usize {
        const BLOCK: usize = 256;
        let common = left.len().min(right.len());
        let mut counted = 0usize;
        for (a, b) in left[..common]
            .chunks(BLOCK)
            .zip(right[..common].chunks(BLOCK))
        {
            if a != b {
                counted += a.iter().zip(b).filter(|(x, y)| x != y).count();
                if counted >= cap {
                    return cap;
                }
            }
        }
        counted
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
    fn the_header_state_never_runs_under_the_pause_control() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let roles = Roles::from_theme(Theme::default());
        let specs = [
            SettingsPreviewSpec::default().with_phase(300),
            SettingsPreviewSpec::default()
                .with_phase(300)
                .with_paused(true),
            SettingsPreviewSpec::default()
                .with_phase(900)
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "sparkle")
                .with_style_trial(Some(StyleTrial::Keyboard)),
        ];
        let mut checked = 0;
        for scale in [1.0_f32, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for width in [254.5_f32, 420.0, 720.0] {
                let rect = LogicalRect::new(16.0, 40.0, width, 220.0);
                for spec in &specs {
                    assert!(spec.pause_control(), "fixture: a looping preview");
                    let header_bottom = spec.terminal_rect(rect).y;
                    let mut prims = Vec::new();
                    spec.paint(&mut prims, rect, Theme::default(), roles);
                    let slot = rect.right() - PREVIEW_INSET - PAUSE_GLYPH_W;
                    let glyph_left = prims
                        .iter()
                        .filter_map(|prim| match prim {
                            DrawPrim::Line {
                                x1,
                                x2,
                                y1,
                                y2,
                                width,
                                ..
                            } if y1.max(*y2) <= header_bottom && x1.min(*x2) >= slot - 0.01 => {
                                Some(x1.min(*x2) - width * 0.5)
                            }
                            _ => None,
                        })
                        .fold(f32::MAX, f32::min);
                    assert!(glyph_left < f32::MAX, "the preview draws its control");
                    let (x, state, px) = prims
                        .iter()
                        .filter_map(|prim| match prim {
                            DrawPrim::Text {
                                x,
                                baseline,
                                s,
                                px,
                                face,
                                ..
                            } if *face == TextFace::UiBold && *baseline <= header_bottom => {
                                Some((*x, s.clone(), *px))
                            }
                            _ => None,
                        })
                        .next_back()
                        .expect("the header says its state");
                    let state_width =
                        crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, px);
                    assert!(
                        x + state_width + 2.0 <= glyph_left,
                        "{scale}x, {width}pt: {state:?} ends at {:.1}, the control starts at \
                         {glyph_left:.1}",
                        x + state_width
                    );
                    let unreserved = header_state_x(rect, PREVIEW_INSET, 0.0, state_width);
                    assert!(
                        unreserved + state_width > glyph_left,
                        "control: {state:?} with no room kept for the control must run under it"
                    );
                    checked += 1;
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
        assert_eq!(checked, 18);
    }

    #[test]
    fn a_preview_header_paints_every_word_it_chooses_at_every_width() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let _ = compiled(SettingsPreviewSpec::default());
        let roles = Roles::from_theme(Theme::default());
        let comet = CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Comet,
            trail_style_raw: Arc::from("comet"),
            ..CursorPreviewSpec::default()
        };
        let specs: Vec<(&str, SettingsPreviewSpec)> = vec![
            (
                "Rainbow kitty",
                SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty pet"))
                    .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow kitty pet"),
            ),
            (
                "Rainbow underline",
                SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty underline"))
                    .with_focus(
                        crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                        "rainbow kitty underline",
                    )
                    .with_style_trial(Some(StyleTrial::Pointer)),
            ),
            (
                "Comet",
                SettingsPreviewSpec::cursor(comet.clone())
                    .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet"),
            ),
            (
                "1 cell",
                SettingsPreviewSpec::cursor(CursorPreviewSpec { length: 1, ..comet })
                    .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_LENGTH, "1")
                    .with_focus_label("Trail length"),
            ),
            (
                "Automatic (SF Mono)",
                SettingsPreviewSpec::typography(13.0)
                    .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
            ),
            (
                "Catppuccin Mocha",
                SettingsPreviewSpec::appearance(13.0)
                    .with_focus(crate::prefs::EDIT_THEME, "Catppuccin Mocha")
                    .with_theme_sample(true),
            ),
            (
                "Off",
                SettingsPreviewSpec::appearance(13.0)
                    .with_focus(crate::prefs::EDIT_SELECTION_INACTIVE, "false")
                    .with_focus_label("Dim selection when unfocused"),
            ),
        ];
        let squash = |text: &str| text.replace("-", "").split_whitespace().collect::<String>();
        let mut checked = 0;
        for scale in [1.0_f32, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for (value, spec) in &specs {
                for width in [
                    96.0_f32, 122.0, 146.0, 150.0, 206.0, 254.0, 320.0, 420.0, 720.0,
                ] {
                    let spec = spec.clone().with_measure_width(width);
                    let height = spec.minimum_height(48.0);
                    let rect = LogicalRect::new(10.0, 10.0, width, height);
                    let mut prims = Vec::new();
                    spec.paint(&mut prims, rect, Theme::default(), roles);
                    let header_bottom = spec.terminal_rect(rect).y;
                    let words = prims
                        .iter()
                        .filter_map(|prim| match prim {
                            DrawPrim::Text {
                                s,
                                x,
                                px,
                                face,
                                baseline,
                                ..
                            } if *baseline <= header_bottom => Some((s.clone(), *x, *px, *face)),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let context = format!("{value} at {width}pt, {scale}x");
                    assert!(!words.is_empty(), "{context}: the header says something");
                    for (text, x, px, face) in &words {
                        assert!(!text.contains('…'), "{context}: {text:?} is cut");
                        let right = x + crate::tray_raster::ui_text_width_for(*face, text, *px);
                        assert!(
                            right <= rect.right() - PREVIEW_INSET + 0.5,
                            "{context}: {text:?} runs to {right:.1}"
                        );
                    }
                    let said = squash(
                        &words
                            .iter()
                            .map(|(text, ..)| text.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                    );
                    assert!(
                        said.contains(&squash(value)),
                        "{context}: the value is painted whole in {words:?}"
                    );
                    let preview = preview_node("preview", spec, height);
                    let tree = UiTree::new(
                        UiNode::new(
                            "root",
                            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                        )
                        .layout(Layout::column().padding(Insets::all(10.0)))
                        .children(vec![
                            preview.layout(
                                Layout::default()
                                    .width(Length::Fixed(width))
                                    .height(Length::Fixed(height))
                                    .clipped(),
                            ),
                        ]),
                    )
                    .compile(LogicalRect::new(0.0, 0.0, width + 40.0, height + 40.0))
                    .expect("compiles");
                    assert!(
                        tree.fit_failures().is_empty(),
                        "{context}: {:?}",
                        tree.fit_failures()
                    );
                    checked += 1;
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
        assert_eq!(checked, 3 * 7 * 9);

        let long = "Pneumonoultramicroscopicsilicovolcanoconiosis".repeat(4);
        let pathological = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, long.as_str());
        let header = pathological.header_in(LogicalRect::new(0.0, 0.0, 122.0, 44.0));
        assert!(
            header.lines.is_empty(),
            "too short for its lines: {header:?}"
        );
        let tree = UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column())
            .children(vec![
                preview_node("preview", pathological, 44.0).layout(
                    Layout::default()
                        .width(Length::Fixed(122.0))
                        .height(Length::Fixed(44.0))
                        .clipped(),
                ),
            ]),
        )
        .compile(LogicalRect::new(0.0, 0.0, 160.0, 100.0))
        .expect("compiles");
        assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
    }

    #[test]
    fn a_preview_header_never_takes_its_specimens_least_row() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let _ = compiled(SettingsPreviewSpec::default());
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        let roles = Roles::from_theme(Theme::default());
        let specs = [
            SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty pet"))
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow kitty pet"),
            SettingsPreviewSpec::typography(12.0)
                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
        ];
        let mut squeezed = 0;
        for scale in [1.0_f32, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for spec in &specs {
                for width in [122.0_f32, 150.0, 206.0] {
                    let spec = spec
                        .clone()
                        .with_prepared_font(prepared.clone())
                        .with_measure_width(width);
                    let full = spec.normalized().header_layout(width);
                    let band = HEADER_BAND;
                    let body = spec.normalized().least_body();
                    let least = (band + body).ceil();
                    let most = spec.minimum_height(0.0).max(full.height + body).ceil();
                    let mut height = least;
                    while height <= most {
                        let rect = LogicalRect::new(10.0, 10.0, width, height);
                        let context =
                            format!("{} at {width}x{height}, {scale}x", spec.semantic_label());
                        let mut prims = Vec::new();
                        spec.paint(&mut prims, rect, Theme::default(), roles);
                        let rows = prims
                            .iter()
                            .find_map(|prim| match prim {
                                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input.rows),
                                _ => None,
                            })
                            .unwrap_or(0);
                        assert!(rows >= 1, "{context}: the specimen paints a row");
                        let header_bottom = spec.terminal_rect(rect).y;
                        let words: Vec<String> = prims
                            .iter()
                            .filter_map(|prim| match prim {
                                DrawPrim::Text { s, baseline, .. }
                                    if *baseline <= header_bottom
                                        && s != spec.scene.badge_label() =>
                                {
                                    Some(s.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        assert!(
                            words.iter().all(|word| !word.contains('…')),
                            "{context}: {words:?}"
                        );
                        if full.height + body > height + 0.01 {
                            assert!(words.is_empty(), "{context}: {words:?}");
                            squeezed += 1;
                        }
                        if height >= spec.minimum_height(0.0) {
                            assert_eq!(
                                words.join(" "),
                                full.lines.join(" "),
                                "{context}: CONTROL, the whole statement"
                            );
                        }
                        height += 4.0;
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
        assert!(squeezed > 0, "some card holds the lines but not the row");
    }

    /// A card too short for its header's lines drops the words rather than cutting the last
    /// line it has room for ("Comet ·…"): a header says a whole statement or nothing.
    #[test]
    fn a_short_card_drops_its_header_words_and_never_cuts_them() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let _ = compiled(SettingsPreviewSpec::default());
        let roles = Roles::from_theme(Theme::default());
        let comet = CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Comet,
            trail_style_raw: Arc::from("comet"),
            ..CursorPreviewSpec::default()
        };
        let long = "Pneumonoultramicroscopicsilicovolcanoconiosis".repeat(2);
        let specs = [
            SettingsPreviewSpec::cursor(comet.clone())
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet"),
            SettingsPreviewSpec::cursor(comet)
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet")
                .with_style_trial(Some(StyleTrial::Pointer)),
            SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty pet"))
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow kitty pet"),
            SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, long.as_str()),
            SettingsPreviewSpec::typography(13.0)
                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
        ];
        let squash = |text: &str| text.replace('-', "").split_whitespace().collect::<String>();
        let (mut dropped, mut kept) = (0, 0);
        for scale in [1.0_f32, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for spec in &specs {
                for width in [96.0_f32, 122.0, 150.0, 206.0] {
                    let spec = spec.clone().with_measure_width(width);
                    let rungs = spec.normalized().header_rungs();
                    let most = spec.minimum_height(0.0).ceil() as i32 + 8;
                    for height in (24..=most).step_by(2) {
                        let rect = LogicalRect::new(10.0, 10.0, width, height as f32);
                        let context = format!(
                            "{} at {width}x{height}, {scale}x",
                            spec.normalized().focused_value
                        );
                        let header = spec.normalized().header_in(rect);
                        let said = header.lines.join(" ");
                        assert!(!said.contains('\u{2026}'), "{context}: {header:?}");
                        if header.lines.is_empty() {
                            assert!(!header.scene && header.says.is_empty(), "{context}");
                            dropped += 1;
                            continue;
                        }
                        assert!(
                            rungs.contains(&header.says) && squash(&said) == squash(&header.says),
                            "{context}: the lines say one whole statement: {header:?}"
                        );
                        let mut prims = Vec::new();
                        spec.paint(&mut prims, rect, Theme::default(), roles);
                        for prim in &prims {
                            if let DrawPrim::Text { s, .. } = prim {
                                assert!(!s.contains('\u{2026}'), "{context}: paints {s:?}");
                            }
                        }
                        kept += 1;
                    }
                }
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
        assert!(dropped > 0 && kept > 0, "{dropped} dropped, {kept} kept");
    }

    /// A header's first line holds its caption at every text size: the band grows with the
    /// type for every scene (not only the text specimen), and so does the Themes demo's strip.
    #[test]
    fn a_preview_headers_first_line_follows_the_text_scale() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let _ = compiled(SettingsPreviewSpec::default());
        let specs = [
            SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
                .with_focus(crate::prefs::EDIT_CURSOR_STYLE, "block"),
            SettingsPreviewSpec::appearance(13.0)
                .with_focus(crate::prefs::EDIT_THEME, "Catppuccin Mocha"),
            wallpaper_spec(None),
            SettingsPreviewSpec::typography(13.0)
                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
        ];
        for scale in [1.0_f32, 1.5, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            let px = header_caption().get();
            for spec in &specs {
                let header = spec.normalized().header_layout(720.0);
                assert_eq!(header.lines.len(), 1, "{scale}x: {header:?}");
                assert!(
                    header.height + 0.01 >= px + 8.0,
                    "{scale}x {:?}: a {px}px caption in a {}pt band",
                    spec.scene,
                    header.height
                );
            }
            for frame in [DemoFrame::Dots, DemoFrame::Windows] {
                let rect = LogicalRect::new(0.0, 0.0, 420.0, 260.0);
                let strip = DemoStripLayout::of(rect, frame);
                assert!(
                    strip.tab_h(rect) + 0.01 >= strip.title_px + 8.0,
                    "{scale}x {frame:?}: a {}px title in a {}pt tab",
                    strip.title_px,
                    strip.tab_h(rect)
                );
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
    }

    #[test]
    fn a_shortened_header_keeps_the_value_whole() {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
            .with_focus(crate::prefs::EDIT_LOAD_ADAPTIVE_MOTION, "true")
            .with_focus_label("Reduce motion under heavy load");
        let wide = spec.header_layout(720.0);
        assert_eq!(wide.lines.len(), 1);
        assert!(wide.lines[0].starts_with("Reduce motion under heavy load: On"));
        let mut saw = (false, false);
        for width in (80..=720).step_by(8) {
            let header = spec.header_layout(width as f32);
            let said = header.lines.join(" ");
            assert!(!said.contains('…'), "{width}: {said}");
            assert!(
                said.ends_with("On") || said.contains("On ·"),
                "{width}: {said}"
            );
            saw.0 |= header.says == "On";
            saw.1 |= header.says == "Reduce motion under heavy load: On";
        }
        assert!(
            saw.0 && saw.1,
            "the ladder reaches the value alone: {saw:?}"
        );
    }

    #[test]
    fn a_wide_preview_header_says_the_whole_badge() {
        let family = "Iosevka Term Extended Heavy Oblique";
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
            "Font: Iosevka Term Extended Heavy Oblique · Text sample"
        );
        let narrow = spec.header_layout(320.0);
        let said = narrow.lines.join(" ");
        assert!(said.len() < header(720.0).len(), "{said}");
        assert!(said.contains(family) && !said.contains('…'), "{said}");
    }

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
        let _ = compiled(SettingsPreviewSpec::default());
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
        let texts = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Text {
                    x,
                    baseline,
                    s,
                    px,
                    face,
                    ..
                } => Some((*x, *baseline, s.as_str(), *px, *face)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!texts.is_empty(), "font candidate pill text");
        assert!(panel.0 >= terminal.x);
        assert!(panel.1 >= terminal.y);
        assert!(panel.0 + panel.2 <= terminal.right() + 0.01);
        assert!(panel.1 + panel.3 <= terminal.bottom() + 0.01);
        for (x, baseline, text, px, face) in texts {
            assert!(!text.contains('…'), "{text:?}");
            assert!(std::str::from_utf8(text.as_bytes()).is_ok());
            let text_right = x + crate::tray_raster::ui_text_width_for(face, text, px);
            assert!(
                text_right <= panel.0 + panel.2 - 8.0 + 0.01,
                "text {text:?} ends at {text_right:.2} outside pill {panel:?}"
            );
            assert!(
                baseline > panel.1 && baseline < panel.1 + panel.3,
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_narrow_static_header_names_its_candidate_once() {
        let spec =
            SettingsPreviewSpec::appearance(24.0).with_focus(crate::prefs::EDIT_THEME, "Nord");
        assert_eq!(spec.animation(), PreviewAnimation::None);
        for rung in spec.header_rungs() {
            assert_eq!(rung.matches("Nord").count(), 1, "{rung}");
        }
        for width in [140.0_f32, 220.0, 420.0] {
            let said = spec.header_layout(width).lines.join(" ");
            assert_eq!(said.matches("Nord").count(), 1, "{width}: {said}");
        }
    }

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

    #[test]
    fn every_listed_cursor_style_is_a_look_of_its_own_at_every_card_size() {
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
        const PHASES: [u64; 4] = [300, 900, 1_500, 2_100];
        let render = |style: &str, theme: PreviewTerminalTheme, rows: usize, phase: u64| {
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
            let specimen = spec.terminal_specimen(theme.into_theme(), Some(rows));
            crate::tray_raster::rasterize_tray(
                &[DrawPrim::TerminalSpecimen {
                    x: 0.0,
                    y: 0.0,
                    spec: Box::new(specimen),
                }],
                360,
                160,
                1.0,
                [0, 0, 0, 255],
            )
            .0
        };
        let styles: Vec<&str> = crate::native_settings::listed_cursor_style_values().collect();
        for (name, theme) in themes {
            for rows in [2, 5] {
                let frames: Vec<Vec<Vec<u8>>> = styles
                    .iter()
                    .map(|style| {
                        PHASES
                            .iter()
                            .map(|&phase| render(style, theme, rows, phase))
                            .collect()
                    })
                    .collect();
                for (i, a) in styles.iter().enumerate() {
                    for (j, b) in styles.iter().enumerate().skip(i + 1) {
                        let apart = (0..PHASES.len())
                            .map(|p| differences_up_to(&frames[i][p], &frames[j][p], 4 * 41) / 4)
                            .max()
                            .unwrap_or(0);
                        assert!(
                            apart > 40,
                            "{a} and {b} paint only {apart} pixels apart on {name} at {rows} rows"
                        );
                    }
                }
                let default = styles
                    .iter()
                    .position(|style| *style == crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
                    .expect("the default is listed");
                for (p, &phase) in PHASES.iter().enumerate() {
                    assert_eq!(
                        differences(
                            &render("rainbow kitty", theme, rows, phase),
                            &frames[default][p]
                        ),
                        0,
                        "an alias paints as the style it names ({name}, {rows} rows)"
                    );
                }
            }
        }
    }

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

        let pair = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_terminal_theme_identity(PreviewThemeIdentity {
                label: "Nord".to_string(),
                light: false,
            })
            .with_focus(crate::prefs::EDIT_THEME, "dark:Nord,light:GitHub Light");
        assert_eq!(pair.candidate_badge(), "Theme: Nord");
        let sample = pair.clone().with_theme_sample(true);
        assert_eq!(sample.candidate_badge(), "Sample: Nord");
        assert_ne!(sample.paint_fingerprint(), pair.paint_fingerprint());

        let ligatures = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_LIGATURES, "true")
            .with_focus_label("Ligatures");
        for text in [ligatures.candidate_badge(), ligatures.semantic_value()] {
            assert!(!text.to_lowercase().contains("true"), "{text}");
        }
        let line_height = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_LINE_HEIGHT, "1.6")
            .with_focus_label("Line height");
        for text in [line_height.candidate_badge(), line_height.semantic_value()] {
            assert!(text.to_lowercase().contains("line height"), "{text}");
        }
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
        let paper = PreviewTerminalTheme {
            fg: 0x20_2020,
            bg: 0xFA_FAFA,
            ..NORD
        };
        assert!(spoken_palette(paper, None).contains(", light, "));
        assert!(spoken_palette(NORD, None).contains(", dark, "));
    }

    #[test]
    fn a_tried_style_badges_and_speaks_as_unsaved() {
        let spec = |trial: bool| {
            SettingsPreviewSpec::cursor(rainbow_cursor("rainbow dog pet"))
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow dog pet")
                .with_style_trial(trial.then_some(StyleTrial::Keyboard))
        };
        let tried = spec(true);
        let badge = tried.with_state_badge(tried.candidate_badge());
        assert!(badge.starts_with("Trying: Rainbow dog"), "{badge}");
        assert!(
            tried
                .header_rungs()
                .iter()
                .all(|rung| rung.starts_with("Trying:"))
        );
        for width in [90.0_f32, 160.0, 260.0, 520.0] {
            let header = tried.header_layout(width);
            assert!(
                header.lines[0].starts_with("Trying:"),
                "{width}: {header:?}"
            );
        }
        assert!(tried.semantic_value().contains("not saved"));
        let saved = spec(false);
        let badge = saved.with_state_badge(saved.candidate_badge());
        assert!(badge.starts_with("Style: Rainbow dog"), "{badge}");
        assert!(!badge.contains("Trying") && !saved.semantic_value().contains("not saved"));
    }

    #[test]
    fn an_idle_focused_row_is_never_badged_live() {
        let comet = || CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::Comet,
            trail_style_raw: Arc::from("comet"),
            ..CursorPreviewSpec::default()
        };
        let spec = SettingsPreviewSpec::cursor(comet())
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "true")
            .with_focus_label("Bloom")
            .with_focused_effect(Some("Inactive: dark themes only".to_string()));
        let badge = spec.with_state_badge(spec.candidate_badge());
        assert_eq!(badge, "Bloom: On · Inactive: dark themes only");
        assert!(!spec.reads_live());
        assert!(
            spec.header_rungs()
                .iter()
                .all(|rung| !rung.contains("Live"))
        );
        assert!(spec.semantic_value().contains("Inactive: dark themes only"));
        let live = SettingsPreviewSpec::cursor(comet())
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "true")
            .with_focus_label("Bloom");
        assert_eq!(
            live.with_state_badge(live.candidate_badge()),
            "Bloom: On · Live trail"
        );
        assert!(live.reads_live());
        let paused = spec.with_reduced_motion(true);
        assert!(
            !paused.live_state().contains("Inactive"),
            "{}",
            paused.live_state()
        );
    }

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
        let unset = "Default".to_string();
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
    fn automatic_window_samples_follow_the_terminal_palette() {
        let paper = PreviewTerminalTheme {
            fg: 0x33_3333,
            bg: 0xFA_FAFA,
            cursor: 0x35_84E4,
            selection: 0xD0_D7DE,
            ..NORD
        };
        let host = Theme::default();
        let dark_palette = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(NORD);
        let light_palette = dark_palette.clone().with_terminal_theme(paper);
        assert!(!dark_palette.window_sample_is_light(host));
        assert!(light_palette.window_sample_is_light(host));
        let side_words = "matches the terminal theme, currently Light";
        assert!(
            light_palette.semantic_value().contains(side_words),
            "{}",
            light_palette.semantic_value()
        );
        assert!(
            differences(
                &pixels(dark_palette.clone()),
                &pixels(light_palette.clone())
            ) > 40
        );
        let demo = SettingsPreviewSpec {
            terminal_theme: Some(paper),
            ..SettingsPreviewSpec::theme_demo(14.0, 0)
        };
        assert_eq!(
            demo.window_sample_is_light(host),
            light_palette.window_sample_is_light(host)
        );
        let explicit = light_palette
            .clone()
            .with_appearance(AppearancePreviewSpec {
                window_theme: "dark".to_string(),
                ..AppearancePreviewSpec::default()
            });
        assert!(!explicit.window_sample_is_light(host));
        assert!(differences(&pixels(explicit), &pixels(light_palette)) > 40);
    }

    #[test]
    fn window_sample_title_and_edge_hold_contrast_on_both_sides() {
        let white = PreviewTerminalTheme {
            fg: 0x24_292F,
            bg: 0xFF_FFFF,
            ..NORD
        };
        let one_dark = PreviewTerminalTheme {
            fg: 0xAB_B2BF,
            bg: 0x21_252B,
            ..NORD
        };
        for (window_theme, theme, rimmed) in [
            ("light", white, true),
            ("dark", one_dark, true),
            ("light", NORD, false),
            ("dark", white, false),
        ] {
            let context = format!("{window_theme} over #{:06X}", theme.bg);
            let spec = SettingsPreviewSpec::appearance(14.0)
                .with_terminal_theme(theme)
                .with_appearance(AppearancePreviewSpec {
                    window_theme: window_theme.to_string(),
                    ..AppearancePreviewSpec::default()
                });
            let prims = compiled(spec).tray(Theme::default(), 13.0).prims;
            let (bar, fill) = prims
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel {
                        x,
                        y,
                        w,
                        h,
                        radius,
                        fill,
                    } if *radius == 0.0 && (*h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01 => {
                        Some((
                            LogicalRect::new(*x, *y, *w, *h),
                            [fill[0], fill[1], fill[2]],
                        ))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{context}: the window sample's bar"));
            let raster = raster_of(&prims);
            let rgb_at = |x: usize, y: usize| {
                let at = (y * VIEWPORT.width as usize + x) * 4;
                [raster[at], raster[at + 1], raster[at + 2]]
            };
            let page = crate::settings::u32_rgb(theme.bg);
            let line = (bar.y as usize + 8..bar.y as usize + 12)
                .flat_map(|y| (bar.x as usize + 50..bar.right() as usize - 12).map(move |x| (x, y)))
                .map(|(x, y)| rgb_at(x, y))
                .max_by(|a, b| {
                    test_contrast_ratio(*a, fill).total_cmp(&test_contrast_ratio(*b, fill))
                })
                .expect("the title line");
            assert!(
                test_contrast_ratio(line, fill) >= 4.5,
                "{context}: title line {line:?} on {fill:?} is {:.2}:1",
                test_contrast_ratio(line, fill)
            );
            let rim = rgb_at(bar.x as usize, bar.y as usize + 9);
            if rimmed {
                assert!(
                    test_contrast_ratio(rim, page) >= 3.0,
                    "{context}: rim {rim:?} on {page:?} is {:.2}:1",
                    test_contrast_ratio(rim, page)
                );
            }
        }
        let old = vec![
            DrawPrim::Panel {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 19.0,
                radius: 0.0,
                fill: rgba([0xF1, 0xF2, 0xF5], 255),
            },
            DrawPrim::Line {
                x1: 0.0,
                y1: 10.0,
                x2: 40.0,
                y2: 10.0,
                width: 2.0,
                color: rgba(window_chrome_text_color(true), 210),
            },
        ];
        let raster = raster_of(&old);
        let worst = (0..40)
            .map(|x| {
                let at = (10 * VIEWPORT.width as usize + x) * 4;
                [raster[at], raster[at + 1], raster[at + 2]]
            })
            .map(|ink| test_contrast_ratio(ink, [0xF1, 0xF2, 0xF5]))
            .fold(0.0_f32, f32::max);
        assert!(worst < 3.0, "the old line read {worst:.2}:1");
    }

    fn window_sample_tones(spec: SettingsPreviewSpec) -> ([u8; 3], Option<[u8; 3]>, [u8; 3]) {
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 360.0, 156.0),
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
        let bar = |h: f32| (h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01;
        let fill = prims.iter().find_map(|p| match p {
            DrawPrim::Panel {
                h, radius, fill, ..
            } if *radius == 0.0 && bar(*h) => Some(rgb(*fill)),
            _ => None,
        });
        let rim = prims.iter().find_map(|p| match p {
            DrawPrim::Stroke {
                h, width, color, ..
            } if bar(*h) && (*width - 1.0).abs() < 0.01 => Some(rgb(*color)),
            _ => None,
        });
        let line = prims.iter().find_map(|p| match p {
            DrawPrim::Line { width, color, .. } if (*width - 2.0).abs() < 0.01 => Some(rgb(*color)),
            _ => None,
        });
        (
            fill.expect("the sample's bar"),
            rim,
            line.expect("the sample's title line"),
        )
    }

    #[test]
    fn the_demo_selected_tab_wears_the_strips_outline() {
        let rect = LogicalRect::new(0.0, 0.0, 520.0, 300.0);
        let layout = DemoStripLayout::of(rect, DemoFrame::Windows);
        let rimmed = |name: &str| {
            let candidate = PreviewTerminalTheme::from_scheme(
                &aterm_types::scheme::builtin(name).expect("a built-in theme"),
            );
            let strip = crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
                theme: candidate.into_theme(),
                ansi: Some(demo_ansi_hues(&candidate)),
                authored: aterm_messages::tokens::AuthoredChrome::NONE,
            });
            let mut prims = Vec::new();
            paint_demo_windows_strip(
                &mut prims,
                rect,
                &layout,
                &strip,
                crate::settings::u32_rgb(candidate.bg),
                crate::settings::u32_rgb(candidate.fg),
                TypeStep::Caption.px(13.75),
            );
            let rim = prims.iter().any(|prim| {
                matches!(prim, DrawPrim::Stroke { width, color, radius, .. }
                    if *width == 1.0
                        && *radius == DEMO_TAB_RADIUS - 0.5
                        && color[..3] == strip.seam)
            });
            (rim, strip)
        };
        let (light, strip) = rimmed("Default Light");
        assert!(light, "Default Light's tab is outlined: {strip:?}");
        assert!(aterm_messages::ink::contrast(strip.active, strip.band) >= 1.15);
        assert!(!rimmed("Default").0, "Default's tab stands off its band");
        for name in aterm_types::scheme::builtin_names() {
            let (rim, strip) = rimmed(name);
            assert_eq!(
                rim,
                crate::tab_bar::windows_tab_wears_rim(strip.active, strip.band),
                "{name}"
            );
            if crate::tab_bar::bg_is_light(strip.band) {
                assert!(rim, "{name}: every light band's selected tab is outlined");
            }
        }
    }

    #[test]
    fn the_window_sample_wears_the_caption_tone_with_a_rim_on_windows() {
        let theme = |name: &str| {
            PreviewTerminalTheme::from_scheme(
                &aterm_types::scheme::builtin(name).expect("a built-in theme"),
            )
        };
        let contrast = aterm_messages::ink::contrast;
        let (light, dark) = (theme("Default Light"), theme("Default"));
        let band = |candidate: PreviewTerminalTheme| {
            crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
                theme: candidate.into_theme(),
                ansi: Some(demo_ansi_hues(&candidate)),
                authored: aterm_messages::tokens::AuthoredChrome::NONE,
            })
        };
        let page = |candidate: PreviewTerminalTheme| crate::settings::u32_rgb(candidate.bg);
        let stock_light = [0xF1, 0xF2, 0xF5];
        assert!(
            contrast(stock_light, page(light)) < 1.1,
            "control: the stock fill vanishes"
        );
        for (candidate, rimmed) in [(light, true), (dark, false)] {
            let spec = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(candidate);
            let (fill, rim, line) = window_sample_tones(spec);
            assert!(contrast(line, fill) >= 4.5, "{line:?} on {fill:?}");
            let strip = band(candidate);
            if cfg!(windows) {
                assert_eq!(fill, strip.band);
                assert_eq!(rim, rimmed.then_some(strip.seam));
                assert!(contrast(fill, page(candidate)) > 1.1, "{fill:?}");
            } else {
                assert_ne!(fill, strip.band);
                assert_eq!(
                    rim.is_some(),
                    contrast(fill, page(candidate)) < 1.3,
                    "{fill:?}"
                );
                if let Some(rim) = rim {
                    assert!(contrast(rim, page(candidate)) >= 3.0, "{rim:?}");
                }
            }
            if let Some(rim) = rim {
                assert!(contrast(rim, fill) >= 1.5, "{rim:?} on {fill:?}");
                assert!(contrast(rim, page(candidate)) >= 1.5, "{rim:?}");
            }
        }
        if cfg!(windows) {
            let plain = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(light);
            let red = [0xFF, 0x00, 0x00];
            let authored =
                plain
                    .clone()
                    .with_authored_chrome(aterm_messages::tokens::AuthoredChrome {
                        border: Some(red),
                        ..aterm_messages::tokens::AuthoredChrome::NONE
                    });
            assert_eq!(window_sample_tones(authored.clone()).1, Some(red));
            assert_ne!(plain.paint_fingerprint(), authored.paint_fingerprint());
        }
        for (candidate, forced, stock) in [
            (light, "light", stock_light),
            (dark, "dark", [0x20, 0x22, 0x2A]),
        ] {
            let spec = SettingsPreviewSpec::appearance(14.0)
                .with_terminal_theme(candidate)
                .with_appearance(AppearancePreviewSpec {
                    window_theme: forced.to_string(),
                    ..AppearancePreviewSpec::default()
                });
            let (fill, rim, line) = window_sample_tones(spec);
            assert_eq!(fill, stock);
            assert!(contrast(line, fill) >= 4.5);
            assert_eq!(
                rim.is_some(),
                contrast(fill, page(candidate)) < 1.3,
                "{forced}"
            );
            if let Some(rim) = rim {
                assert!(contrast(rim, fill) >= 1.5, "{rim:?} on {fill:?}");
            }
        }
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
    fn a_preview_in_a_fading_strip_fades_as_one_picture() {
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let roles = Roles::from_theme(Theme::default());
        let ground = [0x28, 0x1d, 0x4e, 255];
        let lut = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let encode = |l: f32| {
            let l = l.clamp(0.0, 1.0);
            let e = if l <= 0.003_130_8 {
                l * 12.92
            } else {
                1.055 * l.powf(1.0 / 2.4) - 0.055
            };
            (e * 255.0).round() as i32
        };
        for scene in ["cursor", "appearance"] {
            for scale in [1.0_f32, 2.0] {
                let spec = match scene {
                    "cursor" => {
                        SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).with_phase(780)
                    }
                    _ => SettingsPreviewSpec::appearance(14.0),
                }
                .with_prepared_font(prepared.clone());
                let rect = LogicalRect::new(20.0, 10.0, 420.0, 156.0);
                let (top, bottom) = (rect.y + 60.0, rect.bottom());
                let prims = |fade: bool| {
                    let mut prims = vec![
                        DrawPrim::Panel {
                            x: 0.0,
                            y: 0.0,
                            w: 480.0,
                            h: 180.0,
                            radius: 0.0,
                            fill: ground,
                        },
                        DrawPrim::ClipPush {
                            x: rect.x,
                            y: rect.y,
                            w: rect.width,
                            h: rect.height,
                        },
                    ];
                    if fade {
                        prims.push(DrawPrim::FadePush {
                            y: top,
                            h: bottom - top,
                            from: 1.0,
                            to: 0.0,
                        });
                    }
                    spec.paint(&mut prims, rect, Theme::default(), roles);
                    if fade {
                        prims.push(DrawPrim::FadePop);
                    }
                    prims.push(DrawPrim::ClipPop);
                    prims
                };
                let (whole, pw, _) =
                    crate::tray_raster::rasterize_tray(&prims(false), 480, 180, scale, [0; 4]);
                let (faded, _, _) =
                    crate::tray_raster::rasterize_tray(&prims(true), 480, 180, scale, [0; 4]);
                let (y0, y1) = ((top * scale).ceil() as u32, (bottom * scale) as u32);
                let (x0, x1) = ((rect.x * scale) as u32, (rect.right() * scale) as u32);
                let mut worst = (0, 0, 0);
                for y in y0..y1 {
                    let t =
                        ((y as f32 + 0.5 - top * scale) / ((bottom - top) * scale)).clamp(0.0, 1.0);
                    let opacity = (1.0 - t) * (1.0 - t);
                    for x in x0..x1 {
                        let i = ((y * pw + x) * 4) as usize;
                        for k in 0..3 {
                            let want = encode(
                                lut(ground[k]) * (1.0 - opacity) + lut(whole[i + k]) * opacity,
                            );
                            let off = (i32::from(faded[i + k]) - want).abs();
                            if off > worst.0 {
                                worst = (off, x, y);
                            }
                        }
                    }
                }
                assert!(
                    worst.0 <= 2,
                    "{scene} at {scale}x: the band is off its fade by {} at {:?}",
                    worst.0,
                    (worst.1, worst.2)
                );
            }
        }
    }

    #[test]
    fn a_region_under_an_opaque_specimen_rasters_the_same_bytes_drawing_less() {
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let roles = Roles::from_theme(Theme::default());
        for scene in ["cursor", "appearance"] {
            for scale in [1.0_f32, 2.0] {
                let context = format!("{scene} at {scale}x");
                let spec = match scene {
                    "cursor" => {
                        SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).with_phase(780)
                    }
                    _ => SettingsPreviewSpec::appearance(14.0),
                }
                .with_prepared_font(prepared.clone());
                let mut prims = vec![
                    DrawPrim::Panel {
                        x: 0.0,
                        y: 0.0,
                        w: 480.0,
                        h: 220.0,
                        radius: 0.0,
                        fill: rgba([0x22, 0x26, 0x30], 255),
                    },
                    DrawPrim::Panel {
                        x: 12.0,
                        y: 16.0,
                        w: 456.0,
                        h: 196.0,
                        radius: 12.0,
                        fill: rgba([0x40, 0x46, 0x52], 200),
                    },
                ];
                let rect = LogicalRect::new(20.0, 30.0, 420.0, 156.0);
                spec.paint(&mut prims, rect, Theme::default(), roles);
                let (full, pw, _) =
                    crate::tray_raster::rasterize_tray(&prims, 480, 220, scale, [0, 0, 0, 0]);
                let crop = |region: crate::tray_raster::RasterRect| {
                    let mut out = Vec::new();
                    for row in region.y..region.y + region.height {
                        let start = ((row * pw + region.x) * 4) as usize;
                        out.extend_from_slice(&full[start..start + region.width as usize * 4]);
                    }
                    out
                };
                let tick = spec.tick_rect(rect);
                let crossing = crate::tray_raster::RasterRect {
                    x: (tick.x * scale).floor() as u32,
                    y: (tick.y * scale).floor() as u32,
                    width: ((tick.right() * scale).ceil() - (tick.x * scale).floor()) as u32,
                    height: ((tick.bottom() * scale).ceil() - (tick.y * scale).floor()) as u32,
                };
                let covered = crate::tray_raster::occluded_rect_for_test(&prims, scale, crossing)
                    .unwrap_or_else(|| panic!("{context}: the specimen covers its tick region"));
                let exact = crate::tray_raster::RasterRect {
                    x: covered.0 as u32,
                    y: covered.1 as u32,
                    width: (covered.2 - covered.0) as u32,
                    height: (covered.3 - covered.1) as u32,
                };
                for (name, region) in [("exact", exact), ("crossing", crossing)] {
                    crate::tray_raster::DRAWN_PRIMS.with(|drawn| drawn.set(0));
                    let bytes = crate::tray_raster::rasterize_tray_region(
                        &prims,
                        scale,
                        [0, 0, 0, 0],
                        region,
                    );
                    assert!(
                        bytes == crop(region),
                        "{context} {name}: the culled bytes differ"
                    );
                    if name == "exact" {
                        let drawn = crate::tray_raster::DRAWN_PRIMS.with(std::cell::Cell::get);
                        let drawable = prims
                            .iter()
                            .filter(|prim| {
                                !matches!(
                                    prim,
                                    DrawPrim::ClipPush { .. }
                                        | DrawPrim::ClipPop
                                        | DrawPrim::FadePush { .. }
                                        | DrawPrim::FadePop
                                )
                            })
                            .count() as u64;
                        assert!(
                            drawn < drawable,
                            "{context}: {drawn} of {drawable} prims drawn under the frame"
                        );
                    }
                }
            }
        }

        let spec = SettingsPreviewSpec::appearance(14.0).with_prepared_font(prepared.clone());
        let mut sample = Vec::new();
        spec.paint(
            &mut sample,
            LogicalRect::new(0.0, 0.0, 420.0, 156.0),
            Theme::default(),
            roles,
        );
        let specimen = sample
            .into_iter()
            .find_map(|prim| match prim {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(DrawPrim::TerminalSpecimen {
                    x: 0.0,
                    y: 0.0,
                    spec,
                }),
                _ => None,
            })
            .expect("the prepared specimen");
        let backdrop = [0x22, 0x26, 0x30, 255];
        let prims = vec![
            DrawPrim::Panel {
                x: 0.0,
                y: 0.0,
                w: 32.0,
                h: 16.0,
                radius: 0.0,
                fill: backdrop,
            },
            DrawPrim::ClipPush {
                x: 0.1,
                y: 0.0,
                w: 10.3,
                h: 8.0,
            },
            specimen,
            DrawPrim::ClipPop,
        ];
        let scale = 1.25;
        let (full, stride, _) =
            crate::tray_raster::rasterize_tray(&prims, 32, 16, scale, [0, 0, 0, 0]);
        let region = crate::tray_raster::RasterRect {
            x: 0,
            y: 0,
            width: 14,
            height: 10,
        };
        assert!(
            crate::tray_raster::occluded_rect_for_test(&prims, scale, region).is_some(),
            "the fractional region selects the culling path"
        );
        let patch = crate::tray_raster::rasterize_tray_region(&prims, scale, [0, 0, 0, 0], region);
        for row in 0..region.height as usize {
            let from = row * stride as usize * 4;
            let at = row * region.width as usize * 4;
            assert_eq!(
                &patch[at..at + region.width as usize * 4],
                &full[from..from + region.width as usize * 4],
                "fractional clip row {row} agrees with the full raster"
            );
            assert_eq!(
                &patch[at + 13 * 4..at + 14 * 4],
                &backdrop,
                "the backdrop outside the specimen's actual clip survives"
            );
        }
    }

    #[test]
    fn pinned_specimens_fill_their_card_with_whole_rows() {
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let mut checked = 0;
        for (scene, most) in [
            ("typography", TYPOGRAPHY_SPECIMEN_ROWS),
            ("cursor", CURSOR_SPECIMEN_ROWS),
            ("appearance", APPEARANCE_SPECIMEN_ROWS),
        ] {
            for height in [108.0, 132.0, 156.0] {
                for font_px in [10.0, 12.0, 14.0] {
                    for line_height in [1.0, 1.2] {
                        for dpi in [1.0, 2.0] {
                            let mut spec = match scene {
                                "typography" => SettingsPreviewSpec::typography(font_px)
                                    .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default"),
                                "cursor" => SettingsPreviewSpec {
                                    font_px,
                                    ..SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
                                },
                                _ => SettingsPreviewSpec::appearance(font_px),
                            }
                            .with_prepared_font(prepared.clone());
                            spec.line_height = line_height;
                            spec.terminal_scale = dpi;
                            spec.font_status = "committed renderer font cascade ready".to_string();
                            let context =
                                format!("{scene} {height}pt {font_px}px lh {line_height} {dpi}x");
                            let mut prims = Vec::new();
                            spec.paint(
                                &mut prims,
                                LogicalRect::new(0.0, 0.0, 720.0, height),
                                Theme::default(),
                                Roles::from_theme(Theme::default()),
                            );
                            let Some(terminal) = prims.iter().find_map(|prim| match prim {
                                DrawPrim::Panel {
                                    x, y, w, h, radius, ..
                                } if *radius == 8.0 => Some(LogicalRect::new(*x, *y, *w, *h)),
                                _ => None,
                            }) else {
                                continue;
                            };
                            let Some((y, specimen)) = prims.iter().find_map(|prim| match prim {
                                DrawPrim::TerminalSpecimen { y, spec, .. } => Some((*y, spec)),
                                _ => None,
                            }) else {
                                continue;
                            };
                            let normalized = spec.normalized();
                            let pitch = normalized.row_height_at(specimen.font_px);
                            let rows = specimen.input.rows.min(most);
                            let shown = if scene == "cursor" {
                                ((terminal.height / pitch).floor() as usize).min(most)
                            } else {
                                rows
                            };
                            let bottom = match scene {
                                "typography" => terminal.bottom() - 4.0,
                                "appearance" => appearance_specimen_clip(terminal).bottom(),
                                _ => terminal.bottom(),
                            };
                            let band = bottom - (y + shown as f32 * pitch);
                            assert!(band >= -0.01, "{context}: rows past the body by {band}");
                            if shown < most {
                                assert!(
                                    band < pitch,
                                    "{context}: {band}pt of empty terminal under {shown} rows (pitch {pitch})"
                                );
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 50, "{checked}");
        let text = SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
            .with_prepared_font(prepared.clone());
        let cell = prepared.specimen_cell_height(12.0, 1.0, &[]).unwrap() as f32;
        assert_eq!(text.normalized().row_height_at(12.0), cell);
        let mut prims = Vec::new();
        text.paint(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 720.0, 156.0),
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        let rows = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input.rows),
                _ => None,
            })
            .expect("the text specimen");
        assert!(rows >= 7, "a 156pt text card shows {rows} rows");
    }

    #[test]
    fn color_specimen_shows_fewer_whole_rows_never_cut_ones() {
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
                assert!(specimen.font_px <= font_px, "{font_px}px at {height}pt");
                let row = spec.normalized().row_height_at(specimen.font_px);
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
            let sliver = paint(&spec, LogicalRect::new(0.0, 0.0, 360.0, floor - 1.0));
            assert!(sliver.iter().all(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { .. } => false,
                DrawPrim::Panel { h, radius, .. } => {
                    (*h - APPEARANCE_SWATCH_HEIGHT).abs() > 0.01 && (*radius - 8.0).abs() > 0.01
                }
                _ => true,
            }));
        }
        assert!(
            SettingsPreviewSpec::appearance(12.0)
                .with_prepared_font(prepared)
                .minimum_height(48.0)
                <= 72.0
        );
    }

    #[test]
    fn a_color_sample_fits_its_tile_at_every_theme_type_size() {
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let mut cells = std::collections::HashMap::new();
        let mut cell_of = |specimen: &TerminalSpecimenSpec, font_px: f32| {
            *cells
                .entry((font_px.to_bits(), specimen.line_height.to_bits()))
                .or_insert_with(|| {
                    let mut at = specimen.clone();
                    at.font_px = font_px;
                    let (w, h) = crate::tray_raster::specimen_cell_size_for_test(&at, 1.0)
                        .expect("a prepared font measures");
                    (w as f32, h as f32)
                })
        };
        let inked = |input: &RenderInput| {
            input
                .cells
                .iter()
                .enumerate()
                .map(|(row, cells)| {
                    let text = cells
                        .iter()
                        .rposition(|cell| cell.ch != ' ')
                        .map_or(0, |last| last + 1);
                    if input.cursor_visible && input.cursor_row == row {
                        text.max(input.cursor_col + 1)
                    } else {
                        text
                    }
                })
                .collect::<Vec<_>>()
        };
        let overruns = |columns: &[usize],
                        (cell_w, cell_h): (f32, f32),
                        body: (f32, f32),
                        bar: Option<(f32, f32)>| {
            let (left, right) = body;
            columns
                .iter()
                .enumerate()
                .filter(|&(row, &cols)| {
                    let end = left + cols as f32 * cell_w;
                    let top = row as f32 * cell_h;
                    end > right + 0.01
                        || bar.is_some_and(|(bar_left, bar_bottom)| {
                            top < bar_bottom && end > bar_left - APPEARANCE_WINDOW_SAMPLE_GAP + 0.01
                        })
                })
                .count()
        };
        let host = Theme::default();
        let (mut cases, mut narrowed, mut kept, mut below_bar) = (0, 0, 0, 0);
        for width in [240.0_f32, 300.0, 360.0, 480.0] {
            for height in [150.0_f32, 260.0] {
                for base in [6.0_f32, 9.0, 12.0, 14.0, 18.0, 24.0, 32.0] {
                    for size_scale in [0.8_f32, 1.0, 1.15, 1.25] {
                        for line_height in [0.8_f32, 1.0, 1.4, 2.0] {
                            let font_px = base * size_scale;
                            let mut spec = SettingsPreviewSpec::appearance(font_px)
                                .with_prepared_font(prepared.clone());
                            spec.line_height = line_height;
                            let font_px = spec.normalized().font_px;
                            let mut prims = Vec::new();
                            spec.paint(
                                &mut prims,
                                LogicalRect::new(0.0, 0.0, width, height),
                                host,
                                Roles::from_theme(host),
                            );
                            let Some((x, specimen)) = prims.iter().find_map(|p| match p {
                                DrawPrim::TerminalSpecimen { x, spec, .. } => {
                                    Some((*x, spec.as_ref()))
                                }
                                _ => None,
                            }) else {
                                continue;
                            };
                            let body = prims
                                .iter()
                                .find_map(|p| match p {
                                    DrawPrim::Panel { x, w, radius, .. }
                                        if (*radius - 8.0).abs() < 0.01 =>
                                    {
                                        Some((*x, *x + *w))
                                    }
                                    _ => None,
                                })
                                .expect("the terminal body is drawn");
                            let top = prims
                                .iter()
                                .find_map(|p| match p {
                                    DrawPrim::TerminalSpecimen { y, .. } => Some(*y),
                                    _ => None,
                                })
                                .unwrap();
                            let bar = prims.iter().find_map(|p| match p {
                                DrawPrim::Panel {
                                    x, y, h, radius, ..
                                } if *radius == 0.0
                                    && (*h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01 =>
                                {
                                    Some((*x, *y + *h - top))
                                }
                                _ => None,
                            });
                            assert_eq!(x, body.0, "the specimen starts at the body's edge");
                            if bar.is_some_and(|(_, bottom)| bottom <= 0.0) {
                                below_bar += 1;
                            }
                            let case = format!(
                                "{width}x{height} at {font_px}px, line height {line_height}"
                            );
                            assert!(specimen.font_px <= font_px, "never grows: {case}");
                            let columns = inked(&specimen.input);
                            let fitted = cell_of(specimen, specimen.font_px);
                            assert_eq!(
                                overruns(&columns, fitted, body, bar),
                                0,
                                "every shown row fits: {case} drew {}px",
                                specimen.font_px
                            );
                            cases += 1;
                            if specimen.font_px < font_px {
                                narrowed += 1;
                                let authored = cell_of(specimen, font_px);
                                assert!(
                                    overruns(&columns, authored, body, bar) > 0,
                                    "the authored size overran: {case}"
                                );
                            } else {
                                kept += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            narrowed > 0 && kept > 0 && below_bar > 0,
            "{cases}: {narrowed} narrowed, {kept} kept, {below_bar} below the bar"
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
        let row: String = specimen.input.cells[1].iter().map(|cell| cell.ch).collect();
        assert!(row[..specimen.input.cursor_col].ends_with("cursor"));
        assert!(specimen.input.selection.has_selection());
        let breaking = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_CURSOR_BREAK_LIGATURES, "true");
        let (input, _) = build_terminal_specimen_input(&breaking.normalized(), Theme::default());
        assert_eq!(input.cursor_col, 1);
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

        let bounds = |spec: SettingsPreviewSpec| {
            let control = spec.pause_control();
            let compiled = compiled(spec);
            let node = compiled
                .semantic(&crate::native_ui::UiKey::new("preview"))
                .expect("semantic preview");
            assert_eq!(
                node.role,
                if control {
                    SemanticRole::Button
                } else {
                    SemanticRole::Group
                }
            );
            assert!(node.label.contains("preview"));
            assert!(matches!(
                node.value,
                crate::native_ui::SemanticValue::Text(_)
            ));
            assert!(
                compiled
                    .hits
                    .iter()
                    .all(|hit| hit.action.as_str() == PREVIEW_PAUSE_ACTION),
                "a demonstration is not a fake control: its only activation is the real Pause/Play"
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

        let body = |raw: &'static str| {
            let input = played(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(900),
                Theme::default(),
            );
            let sprite = sprites(&input).first().cloned().expect("a drawn companion");
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            sprite.art.data().hash(&mut hash);
            hash.finish()
        };
        assert_ne!(
            body("rainbow kitty pet"),
            body("rainbow dog pet"),
            "species must change pixels"
        );
    }

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
                let sprite = sprites(&input)
                    .into_iter()
                    .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                    .unwrap_or_else(|| panic!("{raw} at {phase} ms draws its companion"));
                assert!(
                    sprite.x >= caret_x && sprite.x <= caret_x + 2 * cell_w,
                    "{raw} at {phase} ms starts {} px from the caret",
                    sprite.x - caret_x
                );
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
                !sprites(&input).is_empty(),
                "{raw} remains a static resident under Reduced Motion"
            );
        }

        let flying = reduced_input("rainbow kitty flying");
        assert!(
            sprites(&flying).is_empty(),
            "ordinary flying-kitty motion stays suppressed"
        );

        let live_flying = played(
            &SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying")).with_phase(2_300),
            Theme::default(),
        );
        assert!(
            !sprites(&live_flying).is_empty(),
            "negative control: the selected flying companion is drawable outside Reduced Motion"
        );
    }

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
                    let input =
                        CursorScene::new(&spec, Theme::default(), rows).decorate(&base, cell);
                    let sprite = sprites(&input)
                        .into_iter()
                        .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                        .unwrap_or_else(|| panic!("{raw} at {phase} ms is drawn"));
                    (input.rows, sprite)
                };
                let at = format!("{raw} at {phase} ms");
                let (rows, short) = body(Some(1));
                assert_eq!(rows, 2, "{at}: the prompt row and the blank row below it");
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

    fn scene_streams(input: &RenderInput) -> String {
        format!(
            "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
            input.cursor_glow_add,
            input.glow_under,
            input.glow_halo,
            input.fire_patch,
            input.char_fg,
            input.fire_halo,
            input.cursor_trail,
            input.cursor_fill_override,
            sprites(input),
        )
    }

    #[test]
    fn a_scene_frame_is_the_same_however_the_frames_arrive() {
        for raw in ["fire", "rainbow kitty pet", "rainbow kitty flying", "comet"] {
            let at = |phase: u64| {
                played(
                    &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(phase),
                    Theme::default(),
                )
            };
            let alone = scene_streams(&at(2_340));
            let _ = played(
                &SettingsPreviewSpec::cursor(rainbow_cursor("water")).with_phase(100),
                Theme::default(),
            );
            let _ = at(900);
            let _ = at(1_716);
            assert_eq!(scene_streams(&at(2_340)), alone, "{raw}");
        }
    }

    #[test]
    fn the_fire_preview_shows_the_sustained_blaze() {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::Fire,
            trail_style_raw: Arc::from("fire"),
            ..CursorPreviewSpec::default()
        });
        let heat = |input: &RenderInput| {
            input
                .fire_patch
                .iter()
                .map(|patch| (patch.temp, patch.peak_h))
                .max()
                .unwrap_or((0, 0))
        };
        let warm: Vec<(u8, u16)> = (0..CURSOR_SCENE_CYCLE_MS)
            .step_by(KEY_MS as usize)
            .map(|phase| heat(&played(&spec.clone().with_phase(phase), Theme::default())))
            .collect();
        let spec = spec.normalized();
        let (base, _) = build_terminal_specimen_input(&spec, Theme::default());
        let scene = CursorScene::new(&spec, Theme::default(), None);
        let row = prompt_row(&base);
        let base = scroll_previous_line(&base, row);
        let geom = Geom {
            cw: 8,
            ch: 17,
            rows: base.rows,
            cols: base.cols,
            origin_x: 0,
            origin_y: 0,
            win_w: (base.cols * 8) as u16,
            win_h: (base.rows * 17) as u16,
            head: 0,
        };
        let stage = SceneStage {
            base: &base,
            scrolled: &base,
            row,
            line: TypedLine::of(&base.cells[row]),
            geom,
            cursor_style: base.cursor_style,
        };
        let mut player = ScenePlayer::new(0, spec.post_fx.companion_look);
        let mut cold = Vec::new();
        for at in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
            scene.step(&mut player, &stage, at, true);
            if at.is_multiple_of(KEY_MS) {
                cold.push(
                    player
                        .step
                        .fire_patch
                        .iter()
                        .map(|patch| (patch.temp, patch.peak_h))
                        .max()
                        .unwrap_or((0, 0)),
                );
            }
        }
        // The typed keys' blaze, as the checks below read it: at the last key the cursor's
        // lead cell can stand past the end of the line above, and a flame there rises into
        // the air, whose floor is as hot from a cold start (the PowerShell line, a column
        // shorter than the Mac's, burns 183 there both ways).
        let keys = CURSOR_SCENE_KEYS as usize;
        let hottest = |heat: &[(u8, u16)]| heat[1..keys].iter().map(|(temp, _)| *temp).max();
        // Up to the Tab the loop types into a forge it has kept hot; the Tab's leap is a
        // meteor whose strike heats even a cold forge for a moment, so the sustained blaze is
        // read on the keys before it.
        let typed = |heat: &[(u8, u16)]| {
            heat[1..=CURSOR_SCENE_TAB.0]
                .iter()
                .map(|(temp, _)| *temp)
                .max()
        };
        let (warmest, coldest_typing) = (typed(&warm).unwrap_or(0), typed(&cold).unwrap_or(0));
        assert!(
            hottest(&warm).unwrap_or(0) >= 120,
            "the blaze reaches orange metal: {warm:?}"
        );
        assert!(
            warmest > coldest_typing + 10,
            "the typed line burns warmer than a cold start's: {warm:?} against {cold:?}"
        );
        assert!(
            (1..keys).all(|key| warm[key].0 + 8 >= cold[key].0),
            "{warm:?} against {cold:?}"
        );
        assert!(
            warm[1..keys]
                .iter()
                .zip(&cold[1..keys])
                .any(|(w, c)| w.1 > c.1)
        );
    }

    fn card_pixels(spec: SettingsPreviewSpec, height: f32) -> Vec<u8> {
        let _ = compiled(SettingsPreviewSpec::default());
        let prepared =
            crate::tray_raster::prepared_semantic_font_for_direct_view_test(&spec.font_candidate);
        let tree = UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(10.0)))
            .children(vec![preview_node(
                "preview",
                spec.with_prepared_font(prepared),
                height,
            )]),
        )
        .compile(VIEWPORT)
        .expect("preview compiles");
        raster_of(&tree.tray(Theme::default(), 13.0).prims)
    }

    /// How many pixels of two rasters differ by more than `levels` in some channel.
    fn apart_by(left: &[u8], right: &[u8], levels: u8) -> usize {
        left.chunks(4)
            .zip(right.chunks(4))
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > levels))
            .count()
    }

    /// THE SCENE'S JUMPS SHOW EVERY JUMP KNOB THE TERMINAL WOULD (audit r14 PV7, r14b PV2-1):
    /// on a dark and a light ground, for every style, Trail length (2 against a length past the
    /// scene's reach), Ring on jumps and Phaser's accent paint the card visibly apart on the
    /// frames after the Return or the Tab exactly where the engine says the scene's jumps draw
    /// them (`GlowStyle::jump_reach`, `GlowStyle::reads_knob`), alike while only typing, and a
    /// knob that shows on the Tab shows on the frame a pause holds. Before, the scene's only
    /// completion was a 6-cell hop over letters, which Fire's flame leaves dark (its Length never
    /// showed) and the header's reach ignored the Return.
    #[test]
    fn the_scenes_jumps_show_every_jump_knob() {
        use aterm_effects::cursor_glow::TrailKnob;

        let light = PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
        );
        let tab_ms = (CURSOR_SCENE_TAB.0 as u64 + 1) * KEY_MS;
        let after: Vec<u64> = [MOTION_STEP_MS, 2 * MOTION_STEP_MS, 4 * MOTION_STEP_MS]
            .into_iter()
            .chain((0..3).map(|step| tab_ms + step * MOTION_STEP_MS))
            .chain([5 * MOTION_STEP_MS])
            .collect();
        let typing_only: Vec<u64> = vec![6 * KEY_MS, CURSOR_SCENE_TAB.0 as u64 * KEY_MS];
        let one_row = {
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).normalized();
            spec.header_layout(VIEWPORT.width - 20.0).height + 15.0 + spec.typography_row_height()
        };
        let mut checked = 0;
        for &raw in crate::prefs::CURSOR_TRAIL_STYLES {
            let style = PreviewTrailStyle::parse(raw);
            let Some(glow) = crate::app_config::resolve_trail_style(raw).style else {
                continue;
            };
            for (ground, heights) in [(None, vec![190.0_f32, one_row]), (Some(light), vec![190.0])]
            {
                let base = CursorPreviewSpec {
                    blink: false,
                    trail_style: style,
                    trail_style_raw: Arc::from(raw),
                    ..CursorPreviewSpec::default()
                };
                let on_ground = |cursor: CursorPreviewSpec| {
                    let spec = SettingsPreviewSpec::cursor(cursor);
                    match ground {
                        Some(theme) => spec.with_terminal_theme(theme),
                        None => spec,
                    }
                };
                let scene = on_ground(base.clone());
                let ctx = scene.scene_knob_context();
                let reach = scene.scene_length_reach();
                let long = reach.map_or(512, |reach| reach.ceil() as usize + 8);
                let knobs = [
                    (
                        TrailKnob::Length,
                        reach.is_some(),
                        CursorPreviewSpec {
                            length: 2,
                            ..base.clone()
                        },
                        CursorPreviewSpec {
                            length: long,
                            ..base.clone()
                        },
                    ),
                    (
                        TrailKnob::Ring,
                        glow.reads_knob(TrailKnob::Ring, ctx),
                        CursorPreviewSpec {
                            ring: true,
                            ..base.clone()
                        },
                        CursorPreviewSpec {
                            ring: false,
                            ..base.clone()
                        },
                    ),
                    (
                        TrailKnob::Accent,
                        glow.reads_knob(TrailKnob::Accent, ctx),
                        CursorPreviewSpec {
                            accent: Some(0x00FF_0000),
                            ..base.clone()
                        },
                        CursorPreviewSpec {
                            accent: Some(0x0000_FFFF),
                            ..base.clone()
                        },
                    ),
                ];
                for (knob, law, a, b) in knobs {
                    if knob == TrailKnob::Accent && glow != GlowStyle::Phaser {
                        continue;
                    }
                    for &height in &heights {
                        let frame = |cursor: &CursorPreviewSpec, phase: u64| {
                            card_pixels(on_ground(cursor.clone()).with_phase(phase), height)
                        };
                        let pairs: Vec<(Vec<u8>, Vec<u8>)> = after
                            .iter()
                            .map(|&phase| (frame(&a, phase), frame(&b, phase)))
                            .collect();
                        let apart = pairs
                            .iter()
                            .map(|(x, y)| apart_by(x, y, 32))
                            .max()
                            .unwrap_or(0);
                        let apart16 = pairs
                            .iter()
                            .map(|(x, y)| apart_by(x, y, 16))
                            .max()
                            .unwrap_or(0);
                        let side = if ground.is_some() { "light" } else { "dark" };
                        if law {
                            assert!(
                                apart >= 12 && apart16 >= 40,
                                "{raw} {side}: {knob:?} shows after a jump in a {height}pt card: \
                                 {apart} px more than 32 levels apart, {apart16} more than 16"
                            );
                            for &phase in &typing_only {
                                assert_eq!(
                                    apart_by(&frame(&a, phase), &frame(&b, phase), 32),
                                    0,
                                    "{raw} {side}: {knob:?} is invisible while only typing \
                                     ({phase} ms)"
                                );
                            }
                        } else {
                            assert!(
                                apart < 12,
                                "{raw} {side}: {knob:?} the engine says the scene cannot show \
                                 paints {apart} px apart"
                            );
                        }
                        checked += 1;
                    }
                }
                // What the Tab shows, the frame a pause holds shows.
                let held = |cursor: CursorPreviewSpec| {
                    card_pixels(on_ground(cursor).with_phase(300).with_paused(true), 190.0)
                };
                let tab = scene_jumps()[0];
                let (cell_w, cell_h) = scene.demo_cell_size();
                let tab_reach = glow.jump_reach(
                    tab.rows,
                    tab.cols,
                    cell_h / cell_w.max(1.0),
                    aterm_effects::cursor_glow::KnobContext {
                        over_text: tab.over_letters,
                        ..ctx
                    },
                );
                if tab_reach.is_some_and(|reach| reach > 2.0) {
                    let apart = apart_by(
                        &held(CursorPreviewSpec {
                            length: 2,
                            ..base.clone()
                        }),
                        &held(CursorPreviewSpec {
                            length: long,
                            ..base.clone()
                        }),
                        32,
                    );
                    assert!(
                        apart >= 12,
                        "{raw}: Trail length shows on the paused frame: {apart} px"
                    );
                }
            }
        }
        assert!(checked >= 40, "{checked} knobs checked");
    }

    #[test]
    fn the_scenes_return_scrolls_the_line_up_with_its_light() {
        let cw = 8_i32;
        let reach = |input: &RenderInput, row: usize| -> i32 {
            let quads = input
                .cursor_glow_add
                .iter()
                .chain(&input.glow_under)
                .filter(|quad| usize::from(quad.row) == row && quad.w > 0 && quad.h > 0)
                .map(|quad| i32::from(quad.x) + i32::from(quad.w));
            let halos = input
                .glow_halo
                .iter()
                .filter(|halo| usize::from(halo.row) == row && halo.w > 0)
                .map(|halo| i32::from(halo.x) + i32::from(halo.w));
            let trail = input
                .cursor_trail
                .iter()
                .filter(|cell| cell.row == row && cell.alpha > 0)
                .map(|cell| (cell.col as i32 + 1) * cw);
            quads.chain(halos).chain(trail).max().unwrap_or(0)
        };
        let text = |cells: &[aterm_core::terminal::RenderCell]| -> String {
            cells.iter().map(|cell| cell.ch).collect()
        };
        for raw in ["rainbow kitty pet", "comet"] {
            let cursor = CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::parse(raw),
                trail_style_raw: Arc::from(raw),
                ..CursorPreviewSpec::default()
            };
            let at = |phase: u64| {
                played(
                    &SettingsPreviewSpec::cursor(cursor.clone()).with_phase(phase),
                    Theme::default(),
                )
            };
            let (before, after) = (
                at(CURSOR_SCENE_CYCLE_MS - MOTION_STEP_MS),
                at(MOTION_STEP_MS),
            );
            let row = after.cursor_row;
            assert!(row > 0 && before.cursor_row == row, "{raw}: one prompt row");
            assert!(
                text(&after.cells[row - 1]).contains(CURSOR_SCENE_COMMAND),
                "{raw}: the line just typed is one row up: {:?}",
                text(&after.cells[row - 1])
            );
            let line_end = before.cursor_col as i32 * cw - 2 * cw;
            let caret_x = after.cursor_col as i32 * cw;
            assert!(
                reach(&before, row) >= line_end && reach(&before, row + 1) >= line_end,
                "{raw}, control: the light reaches the typed line's end before the Return"
            );
            if raw == "comet" {
                assert!(
                    reach(&after, row - 1) >= line_end - 4 * cw,
                    "comet: the Return's streak leaves the line's end, one row up: {} px",
                    reach(&after, row - 1)
                );
                assert!(
                    reach(&after, row) < line_end,
                    "comet: no sweep back along the prompt's row: {} px",
                    reach(&after, row)
                );
            } else {
                assert!(
                    reach(&after, row + 1) <= caret_x + 4 * cw,
                    "{raw}: under the fresh prompt light reaches {} px, the caret at {caret_x}",
                    reach(&after, row + 1)
                );
            }
        }
    }

    #[test]
    fn the_preview_blooms_with_the_live_kernel_below_the_prompt_too() {
        let _ = compiled(SettingsPreviewSpec::default());
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        let theme = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
        let render = |bloom: bool| {
            let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::Water,
                trail_style_raw: Arc::from("water"),
                ..CursorPreviewSpec::default()
            })
            .with_terminal_theme(theme)
            .with_phase(2_340)
            .with_prepared_font(prepared.clone());
            spec.post_fx.bloom = bloom;
            spec.post_fx.bloom_strength = aterm_render::bloom::BLOOM_STRENGTH;
            spec.post_fx.sdr_boost = 0.0;
            let spec = spec.normalized();
            let specimen = spec.terminal_specimen(theme.into_theme(), Some(2));
            let cell = crate::tray_raster::specimen_cell_size_for_test(&specimen, 1.0)
                .expect("the fixture renderer forks");
            let pixels = crate::tray_raster::rasterize_tray(
                &[DrawPrim::TerminalSpecimen {
                    x: 0.0,
                    y: 0.0,
                    spec: Box::new(specimen),
                }],
                400,
                80,
                1.0,
                [0, 0, 0, 255],
            )
            .0;
            (pixels, cell)
        };
        let ((lit, (_, cell_h)), (bare, _)) = (render(true), render(false));
        assert_ne!(lit, bare, "the bloom adds light");
        let gain_below: u32 = lit
            .chunks(400 * 4)
            .zip(bare.chunks(400 * 4))
            .skip(2 * cell_h)
            .take(cell_h)
            .flat_map(|(l, b)| l.iter().zip(b))
            .map(|(l, b)| u32::from(l.saturating_sub(*b)))
            .sum();
        assert!(gain_below > 0, "light below the prompt row");
        let (off_a, _) = render(false);
        assert_eq!(off_a, bare);
    }

    #[test]
    fn the_water_preview_draws_its_wake_behind_the_drop() {
        let light = PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
        );
        for (name, theme) in [
            ("Default", Theme::default()),
            ("GitHub Light", light.into_theme()),
        ] {
            let reach = [2_652_u64, 2_756, 2_964, 3_172]
                .into_iter()
                .map(|phase| {
                    let input = played(
                        &SettingsPreviewSpec::cursor(CursorPreviewSpec {
                            blink: false,
                            trail_style: PreviewTrailStyle::Water,
                            trail_style_raw: Arc::from("water"),
                            ..CursorPreviewSpec::default()
                        })
                        .with_phase(phase),
                        theme,
                    );
                    let row = input.cursor_row as u16;
                    let caret_x = input.cursor_col as i32 * 8;
                    input
                        .glow_under
                        .iter()
                        .chain(&input.cursor_glow_add)
                        .filter(|quad| quad.row == row)
                        .filter(|quad| {
                            let c = quad.color;
                            ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) >= 24
                                || quad.alpha >= 24
                        })
                        .map(|quad| caret_x - i32::from(quad.x))
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            assert!(
                reach >= 3 * 8,
                "{name}: the wake reaches {reach} px behind the drop"
            );
        }
    }

    #[test]
    fn a_trail_length_the_scenes_jumps_cannot_show_says_so() {
        let focused = |raw: &'static str, length: usize| {
            SettingsPreviewSpec::cursor(CursorPreviewSpec {
                trail_style: PreviewTrailStyle::parse(raw),
                trail_style_raw: Arc::from(raw),
                length,
                ..CursorPreviewSpec::default()
            })
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_LENGTH, length.to_string())
            .with_phase(900)
            .normalized()
        };
        // Comet's Return lays the whole line it leaves, a cell a column; Fire flies the Tab's
        // leap as a meteor, and the Return's, steep, shorter; Phaser lays nothing off its row,
        // so only the Tab shows its length, short of the cell it leaves.
        let line = CURSOR_SCENE_COMMAND.len();
        let tab = CURSOR_SCENE_TAB.1 - CURSOR_SCENE_TAB.0;
        for (raw, reach) in [("comet", line), ("fire", tab), ("phaser", tab - 1)] {
            let long = focused(raw, reach);
            let says = format!("Preview shows up to {reach} cells");
            assert_eq!(long.live_state(), says, "{raw}");
            let spoken = long
                .clone()
                .with_focus_label("Trail length")
                .semantic_value();
            assert!(
                spoken.contains(&format!("Trail length · {says}.")),
                "{raw}: {spoken}"
            );
            assert!(
                long.header_rungs()
                    .iter()
                    .all(|rung| !rung.contains("Live")),
                "{raw}"
            );
            assert!(!long.reads_live(), "{raw}");
            let cut = focused(raw, reach - 1);
            assert!(cut.reads_live(), "{raw}: {}", cut.live_state());
            let spoken = cut
                .clone()
                .with_focus_label("Trail length")
                .semantic_value();
            assert!(!spoken.contains("Preview shows up to"), "{raw}: {spoken}");
        }
        let one = focused("comet", 1).with_focus_label("Trail length");
        assert!(
            one.header_rungs()[0].starts_with("Trail length: 1 cell ·"),
            "{:?}",
            one.header_rungs()
        );
    }

    /// The heat shimmer is the GPU's refraction over the flames, which the CPU-drawn preview
    /// cannot draw: focused, the header says so instead of calling the trail live (it said
    /// "Live trail" while turning the shimmer on or off changed nothing it drew).
    #[test]
    fn a_focused_heat_shimmer_says_the_preview_cannot_show_it() {
        let focused = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Fire,
            trail_style_raw: Arc::from("fire"),
            ..CursorPreviewSpec::default()
        })
        .with_focus(crate::prefs::EDIT_CURSOR_FIRE_SHIMMER, "true")
        .with_focus_label("Heat shimmer")
        .with_phase(900)
        .normalized();
        assert_eq!(focused.live_state(), SHIMMER_PREVIEW_CAVEAT);
        assert!(!focused.reads_live());
        assert!(
            focused
                .header_rungs()
                .iter()
                .all(|rung| !rung.contains("Live")),
            "{:?}",
            focused.header_rungs()
        );
        assert!(
            focused
                .semantic_value()
                .contains(&format!("Heat shimmer · {SHIMMER_PREVIEW_CAVEAT}.")),
            "{}",
            focused.semantic_value()
        );
    }

    #[test]
    fn a_reduced_preview_keeps_the_looping_previews_floor() {
        use crate::native_appearance::{AppearancePreferences, install_preferences};
        let looping = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty"))
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow kitty")
            .with_phase(300);
        assert!(looping.pause_control(), "fixture: a looping preview");
        let mut wrapped = 0;
        for scale in [1.0_f32, 1.25, 2.0] {
            let _ = install_preferences(AppearancePreferences {
                text_scale: scale,
                ..AppearancePreferences::default()
            });
            for step in 0..=180_u16 {
                let width = 60.0 + f32::from(step) * 2.0;
                let playing = looping.clone().with_measure_width(width);
                let reduced = playing.clone().with_reduced_motion(true);
                assert!(
                    !reduced.pause_control(),
                    "fixture: reduced, nothing to pause"
                );
                assert_eq!(
                    playing.header_layout(width).height,
                    reduced.header_layout(width).height,
                    "{scale}x, {width}pt: {:?} against {:?}",
                    playing.header_layout(width).lines,
                    reduced.header_layout(width).lines
                );
                assert_eq!(
                    playing.minimum_height(48.0),
                    reduced.minimum_height(48.0),
                    "{scale}x, {width}pt"
                );
                wrapped += usize::from(playing.header_layout(width).lines.len() > 1);
            }
        }
        let _ = install_preferences(AppearancePreferences::default());
        assert!(
            wrapped > 0,
            "control: the sweep reaches widths where the header wraps"
        );
    }

    #[test]
    fn a_faces_landing_is_the_same_demonstration() {
        let probe = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty")).with_phase(300);
        let mut painted = probe.clone();
        painted.font_ready_epoch = 7;
        assert!(
            probe.loop_identity().is_some(),
            "fixture: a looping preview"
        );
        assert_eq!(probe.loop_identity(), painted.loop_identity());
        assert_ne!(probe.paint_fingerprint(), painted.paint_fingerprint());
        let other = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow dog")).with_phase(300);
        assert_ne!(probe.loop_identity(), other.loop_identity());
    }

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
                sprites(&early).is_empty(),
                "no kitty yet at {phase} ms: {} sprites",
                sprites(&early).len()
            );
            assert!(
                !sprites(&at("rainbow kitty pet", phase)).is_empty(),
                "negative control: the walking cat is there at {phase} ms"
            );
        }
        assert!(!sprites(&at("rainbow kitty flying", 2_300)).is_empty());
        let diagnostic = played(
            &SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying"))
                .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "")
                .with_phase(0),
            Theme::default(),
        );
        assert!(
            !sprites(&diagnostic).is_empty(),
            "the sprite control previews its kitty at once"
        );
    }

    #[test]
    fn the_glow_is_lifted_by_hdr_glow_or_sdr_glow_boost_never_both() {
        let halo = |display_edr: bool, hdr_glow: bool, sdr_boost: f32| {
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::Phaser,
                ..CursorPreviewSpec::default()
            })
            .with_post_fx(CursorPostFxSpec {
                display_edr,
                hdr_glow,
                sdr_boost,
                ..CursorPostFxSpec::default()
            })
            .normalized();
            let theme = Theme::default();
            let (base, _) = build_terminal_specimen_input(&spec, theme);
            let scene = CursorScene::new(&spec, theme, None);
            let input = scene.decorate(&base, (8, 17));
            let mut frame = aterm_render::Frame {
                width: 8 * input.cols,
                height: 17 * input.rows,
                pixels: vec![theme.bg; 8 * input.cols * 17 * input.rows],
            };
            scene.post_process(&mut frame, &input).unwrap();
            frame.pixels
        };
        assert!(
            halo(true, true, 0.25) != halo(false, false, 0.0),
            "premise: a crown to compare"
        );
        assert_ne!(
            halo(true, true, 0.25),
            halo(true, false, 0.25),
            "EDR: HDR glow"
        );
        assert_eq!(
            halo(true, true, 0.1),
            halo(true, true, 0.9),
            "EDR: not SDR boost"
        );
        assert_eq!(
            halo(false, true, 0.5),
            halo(false, false, 0.5),
            "SDR: not HDR glow"
        );
        assert_ne!(
            halo(false, true, 0.1),
            halo(false, true, 0.9),
            "SDR: SDR boost"
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
        assert!(!sprites(&input).is_empty());
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
            "the cache keys on the authored spelling"
        );
        assert_eq!(cursor.trail_style_label(), "Rainbow kitty");
        assert_eq!(cursor.unknown_trail_spelling(), Some("phasr"));

        let spec = SettingsPreviewSpec::cursor(cursor.clone()).with_phase(900);
        assert_ne!(spec.animation(), PreviewAnimation::None);
        assert!(!spec.audit_value().contains("fails closed"));
        assert!(has_effect_light(&played(&spec, Theme::default())));
        let spoken = spec.semantic_value();
        assert!(
            spoken.contains("Rainbow kitty trail, since aterm.toml's \"phasr\" is not a style"),
            "{spoken}"
        );
        assert!(!spoken.contains("Phasr"), "{spoken}");
        let focused = SettingsPreviewSpec::cursor(cursor)
            .with_phase(900)
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "phasr");
        assert_eq!(
            focused.candidate_badge(),
            "Style: Rainbow kitty (unknown \"phasr\")"
        );

        for (alias, name) in [("comet", "Comet"), ("rainbow dog pet", "Rainbow dog")] {
            let known = rainbow_cursor(alias);
            let known = CursorPreviewSpec {
                trail_style: PreviewTrailStyle::parse(alias),
                trail_style_raw: Arc::from(alias),
                ..known
            };
            assert_eq!(known.trail_style_label(), name, "{alias}");
            assert_eq!(known.unknown_trail_spelling(), None, "{alias}");
        }
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
    }

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
        let missing = missing.with_focus(crate::prefs::EDIT_FONT_FAMILY, "NoSuchFontXYZ");
        assert_eq!(missing.candidate_badge(), "Font: Current font");
        let mut found = missing.clone();
        found.font_status = "candidate renderer font cascade ready".to_string();
        assert_eq!(found.candidate_badge(), "Font: NoSuchFontXYZ");
    }

    #[test]
    fn a_ready_font_has_no_status_pill_and_a_loading_one_does_not_repeat_its_name() {
        let mut spec = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default");
        spec.font_status = "committed renderer font cascade ready".to_string();
        assert_eq!(spec.font_candidate_pill(), None);
        assert!(spec.candidate_badge().contains("Automatic"));

        spec.font_status = "candidate loading".to_string();
        assert_eq!(
            spec.font_candidate_pill().map(|(_, warning)| warning),
            Some(false),
            "the settled host font is no warning capsule"
        );
    }

    #[test]
    fn typography_reserves_whole_candidate_rows_above_its_status_band() {
        let _ = compiled(SettingsPreviewSpec::typography(12.0));
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        let theme = Theme::default();
        for (status, has_pill) in [
            ("committed renderer font cascade ready", false),
            ("candidate loading", true),
        ] {
            for (width, dpi) in [(254.0, 1.0), (592.0, 1.25), (592.0, 2.0), (900.0, 3.0)] {
                for font_px in [12.0, 24.0, 36.0, 72.0] {
                    for line_height in [0.8, 1.0, 2.0] {
                        let mut spec = SettingsPreviewSpec::typography(font_px)
                            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
                            .with_prepared_font(prepared.clone());
                        spec.line_height = line_height;
                        spec.terminal_scale = dpi;
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
                            let body = height - spec.header_height() - 15.0;
                            let row = spec.normalized().typography_row_height();
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
                                assert!(
                                    specimen.as_ref().unwrap().1.input.rows >= TYPOGRAPHY_KEPT_ROWS,
                                    "all five intended rows fit {width}px / {font_px}px / {line_height}"
                                );
                            }
                            if let Some((y, specimen)) = specimen {
                                assert_eq!(
                                    specimen.font_px,
                                    spec.normalized().font_px,
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
                                let dpi = spec.terminal_scale.max(1.0);
                                let row_h = prepared
                                    .specimen_cell_height(specimen.font_px * dpi, line_height, &[])
                                    .unwrap() as f32
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
        let old_terminal_h = 108.0 - 27.0 - 15.0;
        let row_h = prepared.specimen_cell_height(48.0, 1.0, &[]).unwrap() as f32 / 2.0;
        assert!(row_h * TYPOGRAPHY_SPECIMEN_ROWS as f32 > old_terminal_h - 24.0);
    }

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

    #[test]
    fn appearance_is_truthfully_static_at_every_phase() {
        let early = SettingsPreviewSpec::appearance(14.0).with_phase(100);
        let late = SettingsPreviewSpec::appearance(14.0).with_phase(1_700);
        assert_eq!(early.animation(), PreviewAnimation::None);
        assert_eq!(late.animation(), PreviewAnimation::None);
        assert_eq!(early.paint_fingerprint(), late.paint_fingerprint());
        assert_eq!(pixels(early), pixels(late));
    }

    #[test]
    fn only_pixel_visible_subjects_request_animation() {
        let zero_intensity = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Phaser,
            intensity: 0.0,
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
            blink(CURSOR_SCENE_CYCLE_MS - 200).animation(),
            PreviewAnimation::BlinkEdge { after_ms: 730 },
            "the cycle's phase reset is not a visual blink edge"
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

    #[test]
    fn preview_trail_colours_follow_the_cursor_like_a_window() {
        let theme = PreviewTerminalTheme {
            cursor: 0x50_FA7B,
            ..NORD
        };
        use aterm_effects::cursor_fx::TrailColor;
        let drawn = |style: &'static str, color: TrailColor, pinned: bool| {
            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                blink: false,
                trail_style: PreviewTrailStyle::parse(style),
                trail_style_raw: Arc::from(style),
                color,
                cursor_color_pinned: pinned,
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
        let green = |rgb: u32| {
            let (r, g, b) = channels(rgb);
            g > b && g > r
        };
        for style in ["comet", "beam", "sparkle"] {
            for rgb in drawn(style, TrailColor::Automatic, false) {
                assert!(!green(rgb), "{style} keeps its own hue: {rgb:06x}");
            }
            for rgb in drawn(style, TrailColor::Automatic, true) {
                assert!(
                    green(rgb),
                    "{style} wears the pinned cursor colour: {rgb:06x}"
                );
            }
            for rgb in drawn(style, TrailColor::Pinned(0x00E0_218A), false) {
                let (r, g, b) = channels(rgb);
                assert!(r > g && r > b, "{style} draws its set colour: {rgb:06x}");
            }
        }
        for pinned in [false, true] {
            for rgb in drawn("laser", TrailColor::Automatic, pinned) {
                let (r, g, b) = channels(rgb);
                assert!(b > g && r > g, "Laser keeps its violet: {rgb:06x}");
            }
        }
        for style in ["laser", "comet"] {
            for rgb in drawn(style, TrailColor::Cursor, false) {
                assert!(
                    green(rgb),
                    "{style} in the cursor's colour, chosen: {rgb:06x}"
                );
            }
        }
    }

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

    #[test]
    fn the_off_preview_glows_on_every_terminal() {
        let off = |style: PreviewCursorStyle, bg: u32, glow: bool| {
            let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                style,
                blink: false,
                trail_enabled: true,
                trail_style: PreviewTrailStyle::Off,
                trail_style_raw: Arc::from("off"),
                momentum_glow: glow,
                ..CursorPreviewSpec::default()
            });
            spec.terminal_theme = Some(PreviewTerminalTheme::new(
                0x001F_2328,
                bg,
                0x0009_69DA,
                0x0080_8080,
                [0; 16],
            ));
            spec.with_phase(1_500)
        };
        let dark = off(PreviewCursorStyle::Block, 0x0011_1111, true);
        assert_eq!(dark.state_badge(), "Live typing glow");
        assert!(dark.semantic_value().contains("glowing as you type"));
        for style in [PreviewCursorStyle::Block, PreviewCursorStyle::Bar] {
            let light = off(style, 0x00FF_FFFF, true);
            assert_eq!(light.animation(), PreviewAnimation::Continuous);
            assert_eq!(light.state_badge(), "Live typing glow", "{style:?}");
            assert!(light.semantic_value().contains("glowing as you type"));
            let paper = light.terminal_theme.expect("a light terminal").into_theme();
            let input = played(&light, paper);
            assert!(
                input.glow_under.iter().any(|q| q.alpha > 0),
                "{style:?}: the glow inks the paper"
            );
            let unlit = played(&off(style, 0x00FF_FFFF, false), paper);
            assert!(unlit.glow_under.is_empty(), "{style:?}: no glow, no ink");
        }
    }

    fn sample_rows(source: &str) -> Vec<String> {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(source.as_bytes());
        term.cell_frame(24, 80)
            .cells
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_shell_samples_speak_this_platforms_shell() {
        assert_eq!(APPEARANCE_SOURCE == APPEARANCE_SOURCE_PWSH, cfg!(windows));
        assert_eq!(
            CURSOR_SPECIMEN_LINES == CURSOR_SPECIMEN_LINES_PWSH,
            cfg!(windows)
        );
        let color = sample_rows(APPEARANCE_SOURCE_PWSH);
        let cursor = sample_rows(&CURSOR_SPECIMEN_LINES_PWSH.join("\r\n"));
        for (rows, line) in [
            (&color, "PS C:\\src> git status --short"),
            (&color, "PS C:\\src> cargo test"),
            (&cursor, "PS C:\\> cat README.md"),
            (&cursor, "PS C:\\> git add .\\README.md; git commit"),
        ] {
            assert!(rows.iter().any(|row| row == line), "{line}: {rows:#?}");
        }
        for row in color.iter().chain(&cursor) {
            for mark in ["$ ", "~/", "build.sh", "make "] {
                assert!(!row.contains(mark), "{mark} in a PowerShell sample: {row}");
            }
        }
        let sh = sample_rows(APPEARANCE_SOURCE_SH);
        assert!(sh.iter().any(|row| row == "$ git status --short"));
        assert_eq!(
            sample_rows(&CURSOR_SPECIMEN_LINES_SH.join("\r\n"))[4],
            "~/demo $ cat README.md"
        );
        for row in color.iter().chain(&sh) {
            assert!(row.chars().count() <= SPECIMEN_COLS, "{row}");
        }
        let station = aterm_effects::kitty_pet::art_cols(8, 17).ceil() as usize + 2;
        for row in cursor
            .iter()
            .chain(&sample_rows(&CURSOR_SPECIMEN_LINES_SH.join("\r\n")))
        {
            assert!(row.chars().count() + station <= CURSOR_SCENE_COLS, "{row}");
        }
        assert_eq!(
            APPEARANCE_SOURCE_PWSH.split("\r\n").count(),
            APPEARANCE_SPECIMEN_ROWS
        );
        for (lines, prompt) in [
            (CURSOR_SPECIMEN_LINES_PWSH, "PS C:\\> "),
            (CURSOR_SPECIMEN_LINES_SH, "~/demo $ "),
        ] {
            let mut term = aterm_core::terminal::Terminal::new(2, CURSOR_SCENE_COLS as u16);
            term.process(lines[lines.len() - 1].as_bytes());
            let input = term.cell_frame(2, CURSOR_SCENE_COLS);
            let line = TypedLine::of(&input.cells[0]);
            assert_eq!(
                (line.start, line.end),
                (prompt.len(), prompt.len() + 31),
                "{prompt}"
            );
            let typed: String = input.cells[0][line.start..line.end]
                .iter()
                .map(|cell| cell.ch)
                .collect();
            let command = if prompt.starts_with("PS") {
                "git add .\\README.md; git commit"
            } else {
                "git add README.md && git commit"
            };
            assert_eq!(typed, command, "{prompt}");
        }
    }

    #[test]
    fn terminal_samples_hold_theme_pairs_to_the_tabs_floor() {
        let mut ansi = [0x0020_2020; 16];
        ansi[4] = 0x00F0_F0F0;
        let candidate =
            PreviewTerminalTheme::new(0x0020_2020, 0x00F0_F0F0, 0x0020_2020, 0x00C0_C0C0, ansi);
        let sample = |floor: f32| {
            let spec = SettingsPreviewSpec::appearance(14.0)
                .with_terminal_theme(candidate)
                .with_appearance(AppearancePreviewSpec {
                    indexed_minimum_contrast: floor,
                    ..AppearancePreviewSpec::default()
                });
            build_terminal_specimen_input_rows(&spec, candidate.into_theme(), Some(1))
        };
        let (off, off_key) = sample(1.0);
        let (on, on_key) = sample(crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST.max(4.5));
        assert_ne!(off_key, on_key, "the floor is part of the sample's key");
        let ratio = |input: &RenderInput| {
            let cell = &input.cells[0][0];
            assert_eq!(cell.ch, 'A');
            crate::native_appearance::contrast_ratio(cell.fg, cell.bg)
        };
        assert!(ratio(&off) < 1.1, "unfloored: {}", ratio(&off));
        assert!(ratio(&on) >= 3.0, "floored: {}", ratio(&on));
        assert_eq!(
            AppearancePreviewSpec::default().indexed_minimum_contrast,
            crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST,
            "a sample's floor is the tab's default"
        );
    }

    fn style_scene(style: PreviewTrailStyle, raw: &'static str) -> CursorPreviewSpec {
        CursorPreviewSpec {
            blink: false,
            trail_style: style,
            trail_style_raw: Arc::from(raw),
            momentum_glow: false,
            ..CursorPreviewSpec::default()
        }
    }

    #[test]
    fn a_looping_preview_is_a_focusable_button_in_the_semantic_tree() {
        let key = crate::native_ui::UiKey::new("preview");
        let tree = |spec: SettingsPreviewSpec, focus: Option<&str>, visible: bool| {
            let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                &spec.font_candidate,
            );
            let spec = spec.with_prepared_font(prepared);
            let mut tree = UiTree::new(
                UiNode::new(
                    "root",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                )
                .layout(Layout::column().padding(Insets::all(10.0)))
                .children(vec![
                    preview_node("preview", spec, 190.0),
                    UiNode::new(
                        "other",
                        UiContent::Button(crate::native_ui::Control::new(
                            crate::native_ui::ButtonSpec::new("Other"),
                            crate::native_ui::ActionId::new("other"),
                        )),
                    ),
                ]),
            );
            let focus = focus.map(crate::native_ui::UiKey::new);
            tree.apply_interaction(focus.as_ref(), None, None, visible);
            tree.compile(VIEWPORT).expect("preview compiles")
        };
        let _ = compiled(SettingsPreviewSpec::default());
        let looping = SettingsPreviewSpec::default().with_phase(300);
        assert!(looping.pause_control());
        let ringed = |compiled: &crate::native_ui::CompiledUi| {
            compiled.paint.iter().any(|node| match &node.content {
                UiContent::SettingsPreview(spec) => spec.focus_visible,
                _ => false,
            })
        };

        let focused = tree(looping.clone(), Some("preview"), true);
        let focused_nodes: Vec<_> = focused
            .semantics
            .iter()
            .filter(|node| node.state.is_some_and(|state| state.focused))
            .map(|node| node.key.clone())
            .collect();
        assert_eq!(focused_nodes, vec![key.clone()], "exactly one focused node");
        let node = focused.semantic(&key).expect("the preview");
        assert_eq!(node.role, SemanticRole::Button);
        let state = node.state.expect("a control states itself");
        assert!(state.enabled && state.focused && state.focus_visible);
        assert!(focused.focus_order.contains(&key));
        assert!(ringed(&focused), "keyboard focus rings the card");

        let pointer = tree(looping.clone(), Some("preview"), false);
        let state = pointer.semantic(&key).and_then(|node| node.state);
        assert!(state.is_some_and(|state| state.focused && !state.focus_visible));
        assert!(!ringed(&pointer), "a pointer's focus rings nothing");

        let elsewhere = tree(looping, Some("other"), true);
        let state = elsewhere.semantic(&key).and_then(|node| node.state);
        assert!(state.is_some_and(|state| state.enabled && !state.focused));
        assert!(!ringed(&elsewhere));

        let still = SettingsPreviewSpec::default()
            .with_phase(300)
            .with_reduced_motion(true);
        assert!(!still.pause_control());
        let still = tree(still, Some("preview"), true);
        let node = still.semantic(&key).expect("the preview");
        assert_eq!(node.role, SemanticRole::Group);
        assert!(node.state.is_none() && node.action.is_none());
        assert!(!still.focus_order.contains(&key));
        assert!(!ringed(&still));
    }

    #[test]
    fn pause_control_stills_the_looping_preview() {
        let live = SettingsPreviewSpec::default().with_phase(300);
        assert!(live.pause_control());
        assert_eq!(live.animation(), PreviewAnimation::Continuous);
        let paused = |phase_ms| {
            SettingsPreviewSpec::default()
                .with_phase(phase_ms)
                .with_paused(true)
        };
        assert_eq!(paused(300).animation(), PreviewAnimation::None);
        assert_eq!(
            paused(300).paint_fingerprint(),
            paused(1_900).paint_fingerprint()
        );
        assert_eq!(pixels(paused(300)), pixels_recomputed(paused(1_900)));
        assert_ne!(
            paused(300).state_badge(),
            live.state_badge(),
            "the header says the loop is held"
        );
        assert_ne!(
            paused(300).semantic_value(),
            live.semantic_value(),
            "the accessible value says the loop is held"
        );
        assert!(
            differences(
                &pixels(live),
                &pixels(SettingsPreviewSpec::default().with_phase(1_900))
            ) > 80,
            "negative control: playing, the phase moves pixels"
        );

        let held = compiled(paused(300));
        let key = crate::native_ui::UiKey::new("preview");
        let node = held.semantic(&key).expect("preview semantics");
        assert_eq!(
            node.action.as_ref().map(crate::native_ui::ActionId::as_str),
            Some(PREVIEW_PAUSE_ACTION)
        );
        assert!(held.focus_order.contains(&key), "keyboard reachable");
        let held_gesture = node
            .description
            .clone()
            .expect("a control describes what activating it does");
        let hit = held
            .hits
            .iter()
            .find(|hit| hit.key == key)
            .expect("a hit target");
        assert!(
            hit.rect.width >= 96.0 && hit.rect.height >= 96.0,
            "{:?}",
            hit.rect
        );
        let playing = compiled(SettingsPreviewSpec::default().with_phase(300));
        let playing_gesture = playing
            .semantic(&key)
            .and_then(|node| node.description.clone())
            .expect("a control describes what activating it does");
        assert_ne!(
            held_gesture, playing_gesture,
            "the gesture flips with the state: play when held, pause when playing"
        );

        let blink_only = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_enabled: false,
            trail_style: PreviewTrailStyle::Off,
            ..CursorPreviewSpec::default()
        })
        .with_phase(100)
        .with_paused(true);
        assert!(!blink_only.pause_control());
        assert!(matches!(
            blink_only.animation(),
            PreviewAnimation::BlinkEdge { .. }
        ));
        assert!(
            !SettingsPreviewSpec::default()
                .with_reduced_motion(true)
                .pause_control()
        );
    }

    #[test]
    fn a_paused_preview_holds_the_trail_mid_line() {
        let spec = |phase| {
            SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Comet, "comet"))
                .with_phase(phase)
                .with_paused(true)
        };
        let held = played(&spec(300), Theme::default());
        assert_eq!(
            scene_streams(&held),
            scene_streams(&played(&spec(3_900), Theme::default())),
            "one frame at every phase"
        );
        assert!(has_effect_light(&held), "the held frame shows the trail");
        let (base, _) = build_terminal_specimen_input(&spec(300).normalized(), Theme::default());
        let line = TypedLine::of(&base.cells[prompt_row(&base)]);
        assert!(
            held.cursor_col > line.start && held.cursor_col < line.end,
            "the command is part-typed at the hold: caret {} in {}..{}",
            held.cursor_col,
            line.start,
            line.end
        );
        let still = played(
            &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Comet, "comet"))
                .with_phase(300)
                .with_reduced_motion(true),
            Theme::default(),
        );
        assert!(
            !has_effect_light(&still),
            "control: the still line carries no light"
        );
    }

    #[test]
    fn laser_preview_caret_is_the_bolt() {
        let at = |style, raw| {
            played(
                &SettingsPreviewSpec::cursor(style_scene(style, raw)).with_phase(900),
                Theme::default(),
            )
        };
        let laser = at(PreviewTrailStyle::Laser, "laser");
        assert_eq!(
            laser.cursor_effect_style_override,
            Some(aterm_core::terminal::CursorStyle::Bolt)
        );
        assert!(laser.cursor_visible);
        assert_ne!(
            at(PreviewTrailStyle::Lumen, "lumen").cursor_effect_style_override,
            Some(aterm_core::terminal::CursorStyle::Bolt),
            "negative control: only the laser owns the bolt"
        );
    }

    #[test]
    fn fire_preview_caret_is_the_forge_fill() {
        let fire = played(
            &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Fire, "fire"))
                .with_phase(1_200),
            Theme::default(),
        );
        let fill = fire.cursor_fill_override.expect("fire owns the caret body");
        assert_ne!(fill, Theme::default().cursor & 0x00FF_FFFF);
        let (r, b) = ((fill >> 16) & 0xFF, fill & 0xFF);
        assert!(r > b, "the forge caret is warm metal: {fill:#08x}");
        assert_eq!(
            played(
                &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Classic, "classic"))
                    .with_phase(1_200),
                Theme::default(),
            )
            .cursor_fill_override,
            None,
            "negative control: a style with no caret body leaves the caret alone"
        );
    }

    #[test]
    fn light_theme_rainbow_pet_preview_shows_its_ribbon() {
        let github_light = PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin("GitHub Light").expect("built-in GitHub Light"),
        );
        let at = |cursor| {
            let spec = SettingsPreviewSpec::cursor(cursor)
                .with_phase(1_100)
                .with_terminal_theme(github_light);
            played(&spec, github_light.into_theme())
        };
        let ink = |input: &RenderInput| {
            input
                .glow_under
                .iter()
                .chain(&input.cursor_glow_add)
                .filter(|quad| quad.alpha > 0)
                .count()
        };
        let rainbow = at(CursorPreviewSpec {
            momentum_glow: false,
            ..rainbow_cursor("rainbow kitty pet")
        });
        assert!(ink(&rainbow) > 0, "the light ribbon is ink on the page");
        let off = at(style_scene(PreviewTrailStyle::Off, "off"));
        assert!(!has_effect_light(&off), "control: Off lays no light");
    }

    #[test]
    fn preview_light_is_the_renderer_frame_at_fractional_scales() {
        let specimen_at = |phase_ms| {
            compiled(
                SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty pet"))
                    .with_phase(phase_ms),
            )
            .tray(Theme::default(), 13.0)
            .prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.as_ref().clone()),
                _ => None,
            })
            .expect("cursor specimen")
        };
        let earlier = specimen_at(312);
        let specimen = specimen_at(1_196);
        for scale in [1.25_f32, 1.5] {
            let frame = crate::tray_raster::render_specimen_frame_for_test(&specimen, scale)
                .expect("the specimen renders");
            let logical_w = (frame.width as f32 / scale).ceil() as u32 + 1;
            let logical_h = (frame.height as f32 / scale).ceil() as u32 + 1;
            let blit = |specimen: &crate::widget::TerminalSpecimenSpec| {
                crate::tray_raster::rasterize_tray(
                    &[DrawPrim::TerminalSpecimen {
                        x: 0.0,
                        y: 0.0,
                        spec: Box::new(specimen.clone()),
                    }],
                    logical_w,
                    logical_h,
                    scale,
                    [0, 0, 0, 0],
                )
            };
            let _ = blit(&earlier);
            let (raster, raster_w, _) = blit(&specimen);
            let still = crate::tray_raster::render_specimen_frame_for_test(
                &crate::widget::TerminalSpecimenSpec {
                    motion: None,
                    ..specimen.clone()
                },
                scale,
            )
            .expect("the still specimen renders");
            assert_ne!(
                still.pixels, frame.pixels,
                "{scale}x: the scene reaches the renderer's frame"
            );
            for (row, pixels) in frame.pixels.chunks(frame.width).enumerate() {
                for (col, pixel) in pixels.iter().enumerate() {
                    let i = (row * raster_w as usize + col) * 4;
                    let rgb = [
                        ((pixel >> 16) & 0xff) as u8,
                        ((pixel >> 8) & 0xff) as u8,
                        (pixel & 0xff) as u8,
                    ];
                    assert_eq!(raster[i..i + 3], rgb, "{scale}x pixel ({col},{row})");
                }
            }
        }
    }
}
