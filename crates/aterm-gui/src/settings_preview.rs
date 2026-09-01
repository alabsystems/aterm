// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-native live demonstrations for Settings.
//!
//! A preview is a first-class [`crate::native_ui::UiContent`] node. Its terminal
//! typography uses the renderer-injected monospace face, while cursor motion is
//! produced by the same clockless `aterm-effects` [`CursorGlow`] and
//! [`CursorTrail`] engines as a live terminal. The fixed sample and scripted
//! cursor path are bounded and deterministic; no PTY, platform widget, web view,
//! filesystem access, or wall-clock decision is involved.
//!
//! Optional display-headroom effects are represented by the same bounded SDR
//! tone-map used by capture, so the semantic preview remains portable and
//! truthful on hosts without an HDR panel.

#![allow(
    dead_code,
    reason = "public-to-crate Settings workbench API; route integrations land independently"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_grapheme::{GraphemeClusters, grapheme_display_width};
use aterm_render::{FirePatch, GlowQuad, RainHalo, Theme, TrailCell};

use crate::cursor_glow::{CursorGlow, Geom, GlowStyle, TrailParams};
use crate::cursor_trail::{CursorTrail, TrailConfig};
use crate::native_ui::{Insets, Layout, Length, LogicalRect, UiContent, UiNode};
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{
    DrawPrim, SemanticFontCandidate, SemanticVariation, SpecimenTextBlending, TerminalSpecimenSpec,
    TextFace, TextWeight, rgba, text_prim,
};

const MIN_FONT_PX: f32 = 6.0;
const MAX_FONT_PX: f32 = 32.0;
const MIN_DURATION_MS: u64 = 30;
const MAX_DURATION_MS: u64 = 2_000;
const MAX_LENGTH: usize = 512;
const PHASE_CYCLE_MS: u64 = 2_400;
const MOTION_STEP_MS: u64 = 52;
const CURSOR_BLINK_HALF_MS: u64 = 530;
const WARMUP_STEPS: usize = 7;

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
    WindowTabs,
}

impl PreviewScene {
    const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance preview",
            Self::Typography => "Typography preview",
            Self::CursorMotion => "Cursor and motion preview",
            Self::WindowTabs => "Window and tabs preview",
        }
    }

    const fn badge_label(self) -> &'static str {
        match self {
            Self::Appearance => "COLOR PREVIEW",
            Self::Typography => "TEXT PREVIEW",
            Self::CursorMotion => "CURSOR PREVIEW",
            Self::WindowTabs => "WINDOW PREVIEW",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewCursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
    Hidden,
}

impl PreviewCursorStyle {
    const fn label(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Bar => "bar",
            Self::Underline => "underline",
            Self::Hidden => "hidden",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PreviewTrailStyle {
    /// The defensive fail-closed state. No raw spelling produces it any more —
    /// an unrecognized `cursor_trail_style` falls back to the default and a
    /// malformed `pack:` resolves to [`Self::Custom`] with no pack — so it is
    /// reachable only as a direct construction, and it keeps drawing nothing.
    Invalid,
    Off,
    Lumen,
    #[default]
    Phaser,
    RainbowKitty,
    Sparkle,
    Fire,
    Laser,
    Water,
    Beam,
    Comet,
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

impl PreviewTrailStyle {
    pub(crate) fn parse(value: &str) -> Self {
        let empty = crate::app_config::TrailPackCatalog::default();
        Self::from_resolution(value, crate::app_config::resolve_trail_style(value, &empty))
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
            Some(GlowStyle::Custom) => Self::Custom,
            None if resolved.issue.is_none() => Self::Off,
            None if raw.trim().starts_with("pack:") => Self::Custom,
            None => Self::Invalid,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Invalid => "unavailable",
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
    /// Optional packed `0x00RRGGBB`; `None` follows the live theme/style rule.
    pub(crate) color: Option<u32>,
    /// Optional packed `0x00RRGGBB`; `None` brightens the resolved base colour.
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
    /// Seconds of recent travel the rainbow kitty TYPING WAKE shows — the settings dial's
    /// value, carried here so the live preview lengthens and shortens the plume
    /// exactly as the real terminal will. `0` = wake off.
    pub(crate) wake_persist_s: f32,
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
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 0.7,
            radius: 0.6,
            ring: true,
            wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
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
            color: self.color.map(|color| color & 0x00ff_ffff),
            accent: self.accent.map(|color| color & 0x00ff_ffff),
            duration_ms: self.duration_ms.clamp(MIN_DURATION_MS, MAX_DURATION_MS),
            length: self.length.clamp(1, MAX_LENGTH),
            intensity: finite_or(self.intensity, 0.0).clamp(0.0, 1.0),
            radius: finite_or(self.radius, 0.0).clamp(0.0, 2.0),
            ring: self.ring,
            // Same fail-OFF posture the engine takes: a non-finite dial value
            // must disable the wake, never leak into its length math.
            wake_persist_s: finite_or(self.wake_persist_s, 0.0).clamp(0.0, 1.5),
        }
    }

    /// The selected token when it still names this broad preview style.
    ///
    /// The fallback keeps direct
    /// `CursorPreviewSpec { trail_style, ..Default::default() }` callers honest:
    /// changing the broad style without also changing its raw
    /// token must not accidentally retain the default rainbow-pet semantics.
    ///
    /// AUTHORED, not resolved: this is the text the badge shows and the cache
    /// keys on, so it keeps the user's spelling and case. Ask
    /// [`Self::effective_trail_style_token`] for what the trail actually draws.
    pub(crate) fn effective_trail_style_raw(&self) -> &str {
        let raw = self.trail_style_raw.trim();
        let empty = crate::app_config::TrailPackCatalog::default();
        let resolved = crate::app_config::resolve_trail_style(raw, &empty);
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
    fn effective_trail_style_token(&self) -> &str {
        crate::app_config::effective_trail_style_token(self.effective_trail_style_raw())
    }

    fn resolved_trail_style(&self) -> crate::app_config::ResolvedTrailStyle {
        if self.trail_style == PreviewTrailStyle::Invalid {
            // The terminal defensive state, and the only way in is a direct
            // construction: `from_resolution` no longer answers `Invalid` for
            // anything, because an unrecognized spelling now falls back to the
            // default rather than disabling the trail. A caller that asks for
            // it explicitly still gets nothing drawn.
            return crate::app_config::ResolvedTrailStyle {
                canonical: None,
                style: None,
                pack: None,
                issue: Some(crate::app_config::TrailStyleIssue::Unknown),
            };
        }
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
        let empty = crate::app_config::TrailPackCatalog::default();
        crate::app_config::resolve_trail_style(self.effective_trail_style_raw(), &empty)
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
                wake_persist_s: self.wake_persist_s,
            },
            presentation,
            theme.cursor,
            aterm_render::theme_is_dark(theme.bg),
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

    const fn trail_unavailable(&self) -> bool {
        matches!(self.trail_style, PreviewTrailStyle::Invalid) || self.trail_pack_unavailable()
    }

    const fn has_resolved_trail(&self) -> bool {
        !matches!(
            self.trail_style,
            PreviewTrailStyle::Off | PreviewTrailStyle::Invalid
        ) && !self.trail_pack_unavailable()
    }
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
    pub(crate) const fn new(fg: u32, bg: u32, cursor: u32, selection: u32) -> Self {
        Self {
            fg: fg & 0x00ff_ffff,
            bg: bg & 0x00ff_ffff,
            cursor: cursor & 0x00ff_ffff,
            selection: selection & 0x00ff_ffff,
            ansi: [
                0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xC0C0C0,
                0x808080, 0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
            ],
        }
    }

    pub(crate) fn from_scheme(scheme: &aterm_types::scheme::ColorScheme) -> Self {
        let parts = scheme.to_theme_parts();
        let pack = |color: aterm_types::Rgb| {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        };
        let mut theme = Self::new(parts.fg, parts.bg, parts.cursor, parts.selection);
        theme.ansi = scheme.ansi.map(pack);
        theme
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
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowTabsPreviewSpec {
    pub(crate) columns: usize,
    pub(crate) lines: usize,
    pub(crate) tab_strip_rows: usize,
    pub(crate) show_build_badge: bool,
    /// Whether generated Activity is available as the fallback descriptive lane.
    /// The preview remains a static authored example; this never claims provider
    /// readiness or live inference.
    pub(crate) generate_activity: bool,
    pub(crate) tab_title_format: String,
    pub(crate) window_title_format: String,
}

impl Default for WindowTabsPreviewSpec {
    fn default() -> Self {
        Self {
            columns: 80,
            lines: 24,
            tab_strip_rows: 1,
            show_build_badge: false,
            generate_activity: true,
            tab_title_format: "title-description".to_string(),
            window_title_format: "title-description".to_string(),
        }
    }
}

impl From<Theme> for PreviewTerminalTheme {
    fn from(theme: Theme) -> Self {
        Self::new(theme.fg, theme.bg, theme.cursor, theme.selection)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsPreviewSpec {
    pub(crate) scene: PreviewScene,
    /// Exact candidate terminal type size, independent of native-app text scale.
    pub(crate) font_px: f32,
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
    pub(crate) typography: TypographyPreviewSpec,
    pub(crate) post_fx: CursorPostFxSpec,
    pub(crate) window_tabs: WindowTabsPreviewSpec,
    /// Exact visual field currently authoring the demo and its normalized
    /// uncommitted value. Introspection serializes this literally as `key=value`.
    pub(crate) focused_key: String,
    pub(crate) focused_value: String,
    pub(crate) font_status: String,
    pub(crate) font_ready_epoch: u64,
    pub(crate) prepared_font: crate::tray_raster::PreparedSemanticFont,
    /// Candidate palette for the inner terminal only. `None` follows the host
    /// theme; either way the surrounding Settings card stays host-themed.
    pub(crate) terminal_theme: Option<PreviewTerminalTheme>,
    pub(crate) cursor: CursorPreviewSpec,
    /// Injected deterministic animation phase. Hosts may advance this at their
    /// own cadence; equal specs always produce equal pixels.
    pub(crate) phase_ms: u64,
    /// The already-resolved W11 motion policy. True removes all decorative
    /// motion and freezes a visible cursor instead of retaining a misleading
    /// in-flight trail.
    pub(crate) reduced_motion: bool,
}

impl Default for SettingsPreviewSpec {
    fn default() -> Self {
        Self {
            scene: PreviewScene::CursorMotion,
            font_px: 14.0,
            line_height: 1.0,
            baseline_adjust: 0,
            ligatures: true,
            merged_ligatures: false,
            synthetic_styles: true,
            font_candidate: SemanticFontCandidate::default(),
            appearance: AppearancePreviewSpec::default(),
            system_dark: false,
            typography: TypographyPreviewSpec::default(),
            post_fx: CursorPostFxSpec::default(),
            window_tabs: WindowTabsPreviewSpec::default(),
            focused_key: "cursor_trail_style".to_string(),
            focused_value: crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.to_string(),
            font_status: "host-prepared renderer snapshot unavailable".to_string(),
            font_ready_epoch: 0,
            prepared_font: crate::tray_raster::PreparedSemanticFont::unavailable(
                SemanticFontCandidate::default(),
            ),
            terminal_theme: None,
            cursor: CursorPreviewSpec::default(),
            phase_ms: 720,
            reduced_motion: false,
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

    pub(crate) fn window_tabs(window_tabs: WindowTabsPreviewSpec, font_px: f32) -> Self {
        Self {
            scene: PreviewScene::WindowTabs,
            font_px,
            window_tabs,
            cursor: CursorPreviewSpec {
                trail_enabled: false,
                blink: false,
                ..CursorPreviewSpec::default()
            },
            focused_key: "tab_strip_rows".to_string(),
            focused_value: "1".to_string(),
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

    pub(crate) fn with_terminal_theme(
        mut self,
        terminal_theme: impl Into<PreviewTerminalTheme>,
    ) -> Self {
        self.terminal_theme = Some(terminal_theme.into());
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

    pub(crate) fn with_typography_candidate(mut self, typography: TypographyPreviewSpec) -> Self {
        self.typography = typography;
        self
    }

    pub(crate) fn with_post_fx(mut self, post_fx: CursorPostFxSpec) -> Self {
        self.post_fx = post_fx;
        self
    }

    fn normalized(&self) -> Self {
        Self {
            scene: self.scene,
            font_px: finite_or(self.font_px, 14.0).clamp(MIN_FONT_PX, MAX_FONT_PX),
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
            },
            window_tabs: WindowTabsPreviewSpec {
                columns: self.window_tabs.columns.clamp(1, 1_000),
                lines: self.window_tabs.lines.clamp(1, 1_000),
                tab_strip_rows: self.window_tabs.tab_strip_rows.min(4),
                show_build_badge: self.window_tabs.show_build_badge,
                generate_activity: self.window_tabs.generate_activity,
                tab_title_format: normalized_title_format(&self.window_tabs.tab_title_format),
                window_title_format: normalized_title_format(&self.window_tabs.window_title_format),
            },
            focused_key: self.focused_key.trim().to_string(),
            focused_value: self.focused_value.trim().to_string(),
            font_status: self.font_status.clone(),
            font_ready_epoch: self.font_ready_epoch,
            prepared_font: self.prepared_font.clone(),
            terminal_theme: self.terminal_theme.map(|theme| {
                let mut normalized =
                    PreviewTerminalTheme::new(theme.fg, theme.bg, theme.cursor, theme.selection);
                normalized.ansi = theme.ansi.map(|color| color & 0x00ff_ffff);
                normalized
            }),
            cursor: self.cursor.normalized(),
            phase_ms: self.phase_ms % PHASE_CYCLE_MS,
            reduced_motion: self.reduced_motion,
        }
    }

    /// Return the exact cadence required by the pixels this spec can paint.
    /// Static, hidden, reduced-motion, and zero-intensity combinations return
    /// `None`, preserving aterm's pure-Wait idle contract.
    pub(crate) fn animation(&self) -> PreviewAnimation {
        self.animation_in_place()
    }

    /// `self.normalized().normalized_animation()` evaluated without
    /// materializing the normalized spec.
    ///
    /// The event loop probes the cadence for every window on every
    /// `about_to_wait` turn, and the retained-raster key needs it once per
    /// presented frame; neither wants the ~10 heap allocations `normalized()`
    /// pays to produce a three-variant enum. Only the cursor lane actually
    /// needs normalizing — `CursorPreviewSpec::normalized` allocates nothing
    /// (its one clone is an `Option<Arc>` refcount, kept only for `Custom`) —
    /// because `scene` and `reduced_motion` are copied verbatim and
    /// `millis_until_next_blink_edge` re-applies `% PHASE_CYCLE_MS` itself.
    ///
    /// INVARIANT: this must answer identically to `normalized_animation()` on
    /// the normalized spec, so the scheduler cannot drift from the painter.
    /// Normalizing the cursor is what makes that exact rather than approximate:
    /// a non-finite `intensity` collapses to 0.0 (no motion) here exactly as it
    /// does in paint. The `debug_assert_eq!` is the standing guard.
    fn animation_in_place(&self) -> PreviewAnimation {
        let animation = if self.reduced_motion || self.scene != PreviewScene::CursorMotion {
            PreviewAnimation::None
        } else {
            let cursor = self.cursor.normalized();
            if cursor.trail_enabled && cursor.has_resolved_trail() && cursor.intensity > 0.0 {
                PreviewAnimation::Continuous
            } else if cursor.blink && cursor.style != PreviewCursorStyle::Hidden {
                PreviewAnimation::BlinkEdge {
                    after_ms: self.millis_until_next_blink_edge(),
                }
            } else {
                PreviewAnimation::None
            }
        };
        debug_assert_eq!(
            animation,
            self.normalized().normalized_animation(),
            "preview cadence must not depend on spec normalization"
        );
        animation
    }

    fn normalized_animation(&self) -> PreviewAnimation {
        if self.reduced_motion {
            return PreviewAnimation::None;
        }
        self.animation_without_reduction()
    }

    fn animation_without_reduction(&self) -> PreviewAnimation {
        // Appearance, Typography, and Window/Tabs never paint the cursor-motion
        // lane below. Their shared candidate state still carries cursor fields so
        // switching scenes is lossless, but invisible trail/blink values must not
        // arm the event-loop scheduler or enter retained-paint identity.
        if self.scene != PreviewScene::CursorMotion {
            return PreviewAnimation::None;
        }
        if self.cursor.trail_enabled
            && self.cursor.has_resolved_trail()
            && self.cursor.intensity > 0.0
        {
            return PreviewAnimation::Continuous;
        }
        if self.cursor.blink && self.cursor.style != PreviewCursorStyle::Hidden {
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

    /// A literal in-specimen acknowledgement for font assets and axes. Glyphs
    /// remain the primary proof when a candidate resolves; this capsule closes
    /// the otherwise-confusing unavailable/loading case by saying that the
    /// current face is still being shown. It is deliberately inside the
    /// terminal canvas, not the generic candidate badge above it.
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
        let (state, warning) = if status.contains("unavailable")
            || status.contains("unresolved")
            || status.contains("fallback active")
        {
            // Keep the authored family/axis identity visible inside the bounded
            // capsule. The surrounding specimen already demonstrates the
            // current fallback face; repeating that fact here used the width
            // budget that belongs to the candidate the user is editing.
            ("Unavailable", true)
        } else if self.prepared_font.snapshot.pending
            || status.contains("loading")
            || status.contains("queued")
            || status.contains("ready for host preparation")
        {
            ("Preparing specimen", false)
        } else {
            ("Live specimen", false)
        };
        let value = if self.focused_value.is_empty() {
            "system default"
        } else {
            self.focused_value.as_str()
        };
        Some((format!("{state} · {label}: {value}"), warning))
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
        let candidate = friendly_preview_value(&spec.focused_value);
        match spec.scene {
            PreviewScene::Appearance => {
                let colors = spec.terminal_theme.map_or_else(
                    || "the current terminal colors".to_string(),
                    |theme| {
                        format!(
                            "foreground #{:06X}, background #{:06X}, and cursor #{:06X}",
                            theme.fg, theme.bg, theme.cursor
                        )
                    },
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
                    _ if cfg!(target_os = "linux") => match spec.terminal_theme {
                        Some(candidate) => {
                            if window_sample_light(&spec, candidate.into_theme()) {
                                "window appearance that matches the terminal theme, currently Light"
                            } else {
                                "window appearance that matches the terminal theme, currently Dark"
                            }
                        }
                        None => "window appearance that matches the terminal theme",
                    },
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
                format!("Color theme sample for {candidate}: {colors}, with {window}.")
            }
            PreviewScene::Typography => format!(
                "Text sample for {candidate} at {} pixels; ligatures are {} and synthetic styles are {}.",
                trim_float(spec.font_px),
                if spec.ligatures { "on" } else { "off" },
                if spec.synthetic_styles { "on" } else { "off" },
            ),
            PreviewScene::CursorMotion => {
                if spec.cursor.trail_unavailable() {
                    return "Cursor preview unavailable because the selected trail could not be loaded."
                        .to_string();
                }
                let motion = if spec.reduction_suppresses_motion() {
                    "Static because Reduce Motion is active"
                } else {
                    match spec.normalized_animation() {
                        PreviewAnimation::Continuous => "Live",
                        PreviewAnimation::BlinkEdge { .. } => "Live cursor blink",
                        PreviewAnimation::None => "Static",
                    }
                };
                let trail = if spec.cursor.trail_enabled && spec.cursor.has_resolved_trail() {
                    format!(
                        "{} cursor trail",
                        friendly_preview_value(spec.cursor.effective_trail_style_raw())
                    )
                } else {
                    "cursor with no trail".to_string()
                };
                format!("{motion} preview of the {trail}.")
            }
            PreviewScene::WindowTabs => format!(
                "Window sample: {} columns by {} lines, {} tab-strip {}. Title “release”; Description “shipping”; Activity fallback `running tests` is {}. Formats: tab {}; window {}.",
                spec.window_tabs.columns,
                spec.window_tabs.lines,
                spec.window_tabs.tab_strip_rows,
                if spec.window_tabs.tab_strip_rows == 1 {
                    "row"
                } else {
                    "rows"
                },
                if spec.window_tabs.generate_activity {
                    "shown"
                } else {
                    "off"
                },
                spec.window_tabs.tab_title_format,
                spec.window_tabs.window_title_format,
            ),
        }
    }

    /// Detailed renderer audit projection for introspection and regression
    /// oracles. This deliberately does not back the accessibility value: terms
    /// such as internal primitive names, fingerprints, and tone-map details are
    /// useful to developers but punishing when spoken by assistive technology.
    pub(crate) fn audit_value(&self) -> String {
        let spec = self.normalized();
        let motion = match spec.normalized_animation() {
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
        } else if spec.cursor.trail_style == PreviewTrailStyle::Invalid {
            "The selected trail effect is invalid; the preview fails closed and emits no CursorGlow geometry."
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
            "normalized-candidate {candidate}; renderer preview: {} px monospace, {}× line height, baseline {:+} px; ligatures {}, cursor-break ligatures {}, merged ligatures {}, synthetic styles {}; {}; {font_pill} {terminal_theme}; selection foreground {selection_fg}, minimum contrast {}, selection {}; underline position {:+} thickness {:+} skip descenders {}; text blending {:?}, font thicken {}, stem gamma {}, variation requests {}; {} cursor; blink {}; {} trail; raw/effective motion {}/{} because {}; adaptive motion {} load-shed {}; {motion}. {trail_scope} {post_fx}; Rainbow kitty sprite {:?}; window {} columns × {} lines, tab strip {} row(s), build badge {}; static Smart Titles examples keep stable Title `release` separate from authored Description `shipping`; generated Activity fallback `running tests` is {}; tab format {}; window format {}; no live provider-health claim.",
            trim_float(spec.font_px),
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
            spec.window_tabs.columns,
            spec.window_tabs.lines,
            spec.window_tabs.tab_strip_rows,
            if spec.window_tabs.show_build_badge {
                "shown"
            } else {
                "hidden"
            },
            if spec.window_tabs.generate_activity {
                "shown"
            } else {
                "off"
            },
            spec.window_tabs.tab_title_format,
            spec.window_tabs.window_title_format,
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
    /// same `trail_pack`-only-for-`Custom` rule, same title-format folding, and
    /// the same `phase_ms % PHASE_CYCLE_MS`. Hashing coarser would stale-serve a
    /// retained raster; hashing finer would re-raster identical pixels.
    fn paint_identity_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = aterm_hash::FxHasher::default();
        (self.scene as u8).hash(&mut hash);
        finite_or(self.font_px, 14.0)
            .clamp(MIN_FONT_PX, MAX_FONT_PX)
            .to_bits()
            .hash(&mut hash);
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
        self.window_tabs.columns.clamp(1, 1_000).hash(&mut hash);
        self.window_tabs.lines.clamp(1, 1_000).hash(&mut hash);
        self.window_tabs.tab_strip_rows.min(4).hash(&mut hash);
        self.window_tabs.show_build_badge.hash(&mut hash);
        self.window_tabs.generate_activity.hash(&mut hash);
        normalized_title_format_str(&self.window_tabs.tab_title_format).hash(&mut hash);
        normalized_title_format_str(&self.window_tabs.window_title_format).hash(&mut hash);
        // `PreviewTerminalTheme` is `Copy`, so the normalized candidate is a
        // stack value; only its ANSI ramp needs the mask `new()` already
        // applies to the four named colours.
        self.terminal_theme
            .map(|theme| {
                let mut normalized =
                    PreviewTerminalTheme::new(theme.fg, theme.bg, theme.cursor, theme.selection);
                normalized.ansi = theme.ansi.map(|color| color & 0x00ff_ffff);
                normalized
            })
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
        self.reduced_motion.hash(&mut hash);
        // The phase MUST be reduced before the blink parity is read: 2,400 ms is
        // deliberately not a whole multiple of the 530 ms half-period, so raw
        // phase 4,800 (parity odd) and normalized phase 0 (parity even) disagree.
        // Paint goes through `normalized()`, so the key must too, or two frames
        // that paint differently collide on one retained raster.
        let phase_ms = self.phase_ms % PHASE_CYCLE_MS;
        let cursor_visible = self.reduced_motion
            || !self.cursor.blink
            || (phase_ms / CURSOR_BLINK_HALF_MS).is_multiple_of(2);
        match self.animation_in_place() {
            PreviewAnimation::Continuous => phase_ms.hash(&mut hash),
            PreviewAnimation::BlinkEdge { .. } => cursor_visible.hash(&mut hash),
            PreviewAnimation::None => 0_u64.hash(&mut hash),
        }
        hash.finish() | 1
    }

    fn terminal_specimen(&self, theme: Theme) -> TerminalSpecimenSpec {
        let (input, input_fingerprint) = build_terminal_specimen_input(self, theme);
        TerminalSpecimenSpec {
            input,
            input_fingerprint,
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
            blur: false,
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
        let header_h = 27.0;
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
        let candidate_badge = bounded_badge(&spec.candidate_badge(), 30);
        let mut state = format!("{candidate_badge} · {}", spec.state_badge());
        let mut state_width =
            crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, caption_size.get());
        if scene_width + 8.0 + state_width > available {
            state = format!(
                "{} · {}",
                bounded_badge(&candidate_badge, 20),
                spec.compact_state_badge()
            );
            state_width =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, caption_size.get());
        }
        if state_width > available {
            state = bounded_badge(&spec.compact_state_badge(), 24);
            state_width =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, caption_size.get());
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
                rgba(roles.accent, 255),
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
                if matches!(spec.normalized_animation(), PreviewAnimation::Continuous) {
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
        prims.push(DrawPrim::Panel {
            x: terminal.x,
            y: terminal.y,
            w: terminal.width,
            h: terminal.height,
            radius: 8.0,
            fill: rgba(background, 255),
            blur: false,
        });
        prims.push(DrawPrim::ClipPush {
            x: terminal.x,
            y: terminal.y,
            w: terminal.width,
            h: terminal.height,
        });

        let grid = PreviewGrid::new_for_candidate(
            terminal,
            spec.font_px,
            spec.line_height,
            spec.baseline_adjust,
            &spec.font_candidate,
        );
        if spec.scene == PreviewScene::WindowTabs {
            paint_window_tabs(prims, terminal, &spec, roles, terminal_theme);
            prims.push(DrawPrim::ClipPop);
            return;
        }
        let mut terminal_specimen = spec.terminal_specimen(terminal_theme);
        if spec.scene == PreviewScene::Appearance {
            terminal_specimen.font_px = appearance_specimen_font_px(
                terminal,
                terminal_specimen.font_px,
                terminal_specimen.line_height,
            );
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
        if spec.scene == PreviewScene::Appearance {
            paint_window_theme_sample(prims, terminal, &spec, terminal_theme);
        }
        let effects = spec.effects(grid, terminal_theme);
        if spec.scene == PreviewScene::CursorMotion {
            paint_trail(
                prims,
                grid,
                &effects.trail,
                effects.trail_color,
                terminal_theme.bg,
            );
            paint_effect_under(prims, grid, &effects);
            paint_effect_over(prims, grid, &effects);
            paint_portable_post_fx(prims, grid, &spec, effects.cursor, terminal_theme);
            paint_cursor(prims, grid, &spec, effects.cursor, terminal_theme);
        }
        paint_font_candidate_pill(prims, terminal, &spec, roles);
        prims.push(DrawPrim::ClipPop);
    }

    fn state_badge(&self) -> String {
        if self.cursor.trail_unavailable() {
            "Trail unavailable".to_string()
        } else if self.reduction_suppresses_motion() {
            "Reduced motion".to_string()
        } else {
            match self.normalized_animation() {
                PreviewAnimation::None => match self.scene {
                    PreviewScene::Appearance => "Color sample".to_string(),
                    PreviewScene::Typography => "Text sample".to_string(),
                    PreviewScene::WindowTabs => "Layout sample".to_string(),
                    PreviewScene::CursorMotion => "Static preview".to_string(),
                },
                PreviewAnimation::BlinkEdge { .. } => "Live cursor blink".to_string(),
                PreviewAnimation::Continuous => "Live trail".to_string(),
            }
        }
    }

    fn candidate_badge(&self) -> String {
        let subject = match self.focused_key.as_str() {
            crate::prefs::EDIT_THEME => "Theme",
            crate::prefs::EDIT_WINDOW_THEME => "Window",
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE => "Trail",
            crate::prefs::EDIT_CURSOR_STYLE => "Cursor",
            crate::prefs::EDIT_FONT_FAMILY => "Font",
            crate::prefs::EDIT_FONT_PX => "Size",
            crate::prefs::EDIT_TAB_STRIP_ROWS => "Tabs",
            _ => "Preview",
        };
        format!("{subject}: {}", friendly_preview_value(&self.focused_value))
    }

    /// Compact, user-facing glance label for narrow preview headers.
    fn compact_state_badge(&self) -> String {
        if self.cursor.trail_unavailable() {
            "Trail unavailable".to_string()
        } else if self.reduction_suppresses_motion() {
            "Reduced motion".to_string()
        } else {
            match self.normalized_animation() {
                PreviewAnimation::None => self.candidate_badge(),
                PreviewAnimation::BlinkEdge { .. } => "Live blink".to_string(),
                PreviewAnimation::Continuous => format!(
                    "{} · Live",
                    friendly_preview_value(self.cursor.effective_trail_style_raw())
                ),
            }
        }
    }

    fn effects(&self, grid: PreviewGrid, theme: Theme) -> PreviewEffects {
        let mut frame = PreviewEffects {
            cursor: grid.start_cursor(),
            ..PreviewEffects::default()
        };
        if self.cursor.trail_pack_unavailable()
            || self.reduced_motion
            || !self.cursor.trail_enabled
            || !self.cursor.has_resolved_trail()
            || self.cursor.intensity <= 0.0
            || grid.rows == 0
            || grid.cols == 0
        {
            return frame;
        }

        let glow_config = self.cursor.glow_config(
            theme,
            if self.cursor.style == PreviewCursorStyle::Bar {
                0.08
            } else {
                0.5
            },
        );
        let style = glow_config.style;
        let color = glow_config.color;
        frame.trail_color = color;
        let trail_config = TrailConfig {
            enabled: style == GlowStyle::Comet,
            duration: glow_config.duration,
            max_len: glow_config.length,
            color,
            intensity: glow_config.intensity,
            warmth: 0.72,
        };
        let geometry = Geom {
            cw: grid.cell_w,
            ch: grid.cell_h,
            rows: grid.rows,
            cols: grid.cols,
            origin_x: 0,
            origin_y: 0,
            win_w: u16::try_from(grid.cols.saturating_mul(grid.cell_w)).unwrap_or(u16::MAX),
            win_h: u16::try_from(grid.rows.saturating_mul(grid.cell_h)).unwrap_or(u16::MAX),
            head: 0,
        };
        let mut glow = CursorGlow::default();
        let mut trail = CursorTrail::default();
        let mut base = Instant::now();
        let phase_steps = usize::try_from(self.phase_ms / MOTION_STEP_MS).unwrap_or(0);
        let remainder = self.phase_ms % MOTION_STEP_MS;
        for step in 0..=WARMUP_STEPS.saturating_add(phase_steps) {
            let cursor = scripted_cursor(grid, step);
            if step > 0 {
                glow.note_synthetic_typed(base, 1);
                trail.note_synthetic_typed(base);
            }
            let _ = glow.tick(Some(cursor), base, &glow_config, geometry, &mut frame.glow);
            let _ = trail.tick(Some(cursor), base, &trail_config, &mut frame.trail);
            frame.cursor = cursor;
            if step < WARMUP_STEPS.saturating_add(phase_steps) {
                base += Duration::from_millis(MOTION_STEP_MS);
            }
        }
        if remainder > 0 {
            base += Duration::from_millis(remainder);
            let _ = glow.tick(
                Some(frame.cursor),
                base,
                &glow_config,
                geometry,
                &mut frame.glow,
            );
            let _ = trail.tick(Some(frame.cursor), base, &trail_config, &mut frame.trail);
        }
        frame.halos.extend_from_slice(glow.halos());
        frame.under.extend_from_slice(glow.under_quads());
        frame.patches.extend_from_slice(glow.patches());
        frame
    }
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
    let pill_h = (text_size.get() + 7.0)
        .max(19.0)
        .min((terminal.height - 8.0).max(1.0));
    let max_chars = if terminal.width < 300.0 { 34 } else { 58 };
    let label = bounded_badge(&label, max_chars);
    // The terminal has a 5pt outer inset and the pill has 8pt text padding on
    // both sides. Refit the actual UiBold label to that exact pixel budget;
    // character/grapheme caps alone cannot bound proportional text at 2×.
    let label = bounded_badge_width(&label, (terminal.width - 26.0).max(0.0), text_size.get());
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
        blur: false,
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
/// return the leading inset available to title text. Top Settings and the
/// Advanced Window/Tabs sample deliberately share this primitive so neither
/// surface can claim macOS traffic lights on another platform.
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
                breathe: false,
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

/// The one dark/light classifier for preview terminal palettes — delegates to
/// [`crate::tab_bar::theme_is_dark`] so the window sample and the real chrome
/// can never disagree about a theme's side.
fn theme_bg_is_dark(bg: u32) -> bool {
    crate::tab_bar::theme_is_dark(bg)
}

/// The light/dark side the WINDOW SAMPLE paints for the spec's `window_theme`.
/// Explicit Light/Dark is itself. Auto is the PLATFORM truth: macOS/Windows
/// follow the live system appearance (`system_dark`), while Linux follows the
/// candidate TERMINAL THEME's darkness — the resolution the shipping chrome
/// actually uses there (`chrome_theme_for_apprt`/`chrome_palette_theme`; no
/// live OS-appearance feed exists under winit's Wayland backend).
fn window_sample_light(spec: &SettingsPreviewSpec, terminal_theme: Theme) -> bool {
    match spec.appearance.window_theme.as_str() {
        "light" => true,
        "dark" => false,
        _ if cfg!(target_os = "linux") => !theme_bg_is_dark(terminal_theme.bg),
        _ => !spec.system_dark,
    }
}

const APPEARANCE_SPECIMEN_ROWS: usize = 3;
const APPEARANCE_SWATCH_BOTTOM_INSET: f32 = 10.0;
const APPEARANCE_SWATCH_HEIGHT: f32 = 6.0;
const APPEARANCE_SPECIMEN_SWATCH_GAP: f32 = 4.0;
const APPEARANCE_RENDERER_LINE_ALLOWANCE: f32 = 1.25;

fn appearance_swatch_y(terminal: LogicalRect) -> f32 {
    (terminal.bottom() - APPEARANCE_SWATCH_BOTTOM_INSET).max(terminal.y + 20.0)
}

fn appearance_specimen_clip(terminal: LogicalRect) -> LogicalRect {
    LogicalRect::new(
        terminal.x,
        terminal.y,
        terminal.width,
        (appearance_swatch_y(terminal) - APPEARANCE_SPECIMEN_SWATCH_GAP - terminal.y).max(1.0),
    )
}

fn appearance_specimen_font_px(
    terminal: LogicalRect,
    requested_font_px: f32,
    line_height: f32,
) -> f32 {
    let rows = APPEARANCE_SPECIMEN_ROWS as f32;
    let line_height = line_height.max(0.5);
    let cap = appearance_specimen_clip(terminal).height
        / rows
        / line_height
        / APPEARANCE_RENDERER_LINE_ALLOWANCE;
    requested_font_px.min(cap.max(1.0))
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
    prims.push(DrawPrim::Panel {
        x,
        y: terminal.y,
        w: width,
        h: 19.0,
        radius: 0.0,
        fill: rgba(fill, 255),
        blur: false,
    });
    let title_inset = paint_platform_window_controls(prims, x, terminal.y, 19.0, light);
    prims.push(DrawPrim::Line {
        x1: x + title_inset,
        y1: terminal.y + 9.5,
        x2: terminal.right() - 8.0,
        y2: terminal.y + 9.5,
        width: 2.0,
        color: rgba(window_chrome_text_color(light), 210),
    });

    // The semantic terminal specimen remains the primary renderer proof, but a
    // freshly opened Settings tab may need one asynchronous font-preparation
    // turn before glyphs land. Keep the small Top card useful on that first
    // frame with a renderer-native monospace label and literal ANSI swatches.
    // These use the candidate palette and therefore cannot collapse into the
    // blank background-only sample the old 132pt card produced.
    let candidate = spec
        .terminal_theme
        .unwrap_or_else(|| PreviewTerminalTheme::from(terminal_theme));
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
    let swatch_gap = 3.0;
    let swatch_count = 8.0;
    let swatch_width =
        ((terminal.width - 16.0 - swatch_gap * (swatch_count - 1.0)) / swatch_count).max(2.0);
    let swatch_y = appearance_swatch_y(terminal);
    for (index, color) in candidate.ansi[8..].iter().copied().enumerate() {
        prims.push(DrawPrim::Panel {
            x: terminal.x + 8.0 + index as f32 * (swatch_width + swatch_gap),
            y: swatch_y,
            w: swatch_width,
            h: APPEARANCE_SWATCH_HEIGHT,
            radius: 2.0,
            fill: rgba(crate::settings::u32_rgb(color), 255),
            blur: false,
        });
    }
}

fn packed_rgb(value: u32) -> aterm_types::Rgb {
    aterm_types::Rgb::new(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    )
}

fn build_terminal_specimen_input(
    spec: &SettingsPreviewSpec,
    theme: Theme,
) -> (Arc<aterm_render::RenderInput>, u64) {
    use std::hash::{Hash, Hasher};

    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::{CursorStyle, Terminal};

    const COLS: usize = 32;
    const FULL_SOURCE: &str = concat!(
        "\u{1b}[31mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
        "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
        "\u{1b}[4mgypq_j underline\u{1b}[0m\r\n",
        "CJK 你好  symbols ∑✓♥\r\n",
        "emoji 😀 🚀 👩‍💻"
    );
    const APPEARANCE_SOURCE: &str = concat!(
        "\u{1b}[31mANSI 4 \u{1b}[1mBOLD\u{1b}[0m \u{1b}[2mFAINT\u{1b}[0m\r\n",
        "!= => -> \u{1b}[3mITAL\u{1b}[0m \u{1b}[1;3mB+I\u{1b}[0m cursor\r\n",
        "\u{1b}[4mgypq_j underline\u{1b}[0m"
    );
    let (rows, source) = if spec.scene == PreviewScene::Appearance {
        (APPEARANCE_SPECIMEN_ROWS, APPEARANCE_SOURCE)
    } else {
        (5, FULL_SOURCE)
    };

    // The memo key comes FIRST because it is a total function of this call's
    // inputs — nothing below reads anything that is not hashed here.
    //
    // INVARIANT: every value the build below consults must appear in this hash.
    // A `Continuous` preview re-lowers at 30 fps and only `phase_ms` moves
    // between ticks, which the specimen itself does not read (it enters through
    // `cursor_visible()` alone), so an unhashed input would silently freeze a
    // stale snapshot on glass. Keep this list in step with the build.
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    rows.hash(&mut hash);
    theme.fg.hash(&mut hash);
    theme.bg.hash(&mut hash);
    theme.cursor.hash(&mut hash);
    theme.selection.hash(&mut hash);
    // Scene selects the source above AND gates the kitty layer below.
    (spec.scene as u8).hash(&mut hash);
    spec.terminal_theme
        .map(|candidate| candidate.ansi)
        .hash(&mut hash);
    spec.appearance.selection_foreground.hash(&mut hash);
    spec.appearance.bold_is_bright.hash(&mut hash);
    spec.appearance.faint_opacity.to_bits().hash(&mut hash);
    (spec.cursor.style as u8).hash(&mut hash);
    spec.cursor.blink.hash(&mut hash);
    spec.cursor_visible().hash(&mut hash);
    // The remaining four gate the kitty sprite layer.
    spec.cursor.trail_enabled.hash(&mut hash);
    (spec.cursor.trail_style as u8).hash(&mut hash);
    hash_trimmed_lowercase(spec.cursor.effective_trail_style_raw(), &mut hash);
    spec.reduced_motion.hash(&mut hash);
    spec.focused_key.hash(&mut hash);
    spec.post_fx.kitty_sprite.hash(&mut hash);
    spec.post_fx.kitty_asset.fingerprint().hash(&mut hash);
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
        cursor_style: match spec.cursor.style {
            PreviewCursorStyle::Bar => CursorStyle::SteadyBar,
            PreviewCursorStyle::Underline => CursorStyle::SteadyUnderline,
            PreviewCursorStyle::Hidden | PreviewCursorStyle::Block => CursorStyle::SteadyBlock,
        },
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
    let mut terminal = Terminal::new(rows as u16, COLS as u16);
    let _ = terminal.apply_config(&terminal_config);
    terminal.process(source.as_bytes());
    let mut input = terminal.cell_frame(rows, COLS);
    input.cursor_row = 1;
    // Cursor is inside the `!=` run so cursor-break-ligatures exercises the
    // same shaping split a terminal uses while editing operators.
    input.cursor_col = 1;
    input.cursor_style = terminal_config.cursor_style;
    input.cursor_visible = spec.cursor.style != PreviewCursorStyle::Hidden && spec.cursor_visible();
    input.cursor_color = theme.cursor;
    input.default_bg = theme.bg;
    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(0, 5, SelectionSide::Right);
    selection.complete_selection();
    input.selection = selection;

    if spec.scene == PreviewScene::CursorMotion && spec.cursor.trail_enabled {
        let companion = if spec.focused_key == crate::prefs::EDIT_CURSOR_NYAN_SPRITE {
            // The sprite control previews the asset it edits even when the
            // selected runtime companion is a resident pet. This focused
            // specimen is explicitly diagnostic, never a claim that the
            // flying sprite is active under the pet style.
            (!spec.reduced_motion)
                .then(|| kitty_layer(&spec.post_fx.kitty_asset))
                .flatten()
        } else {
            match spec.cursor.trail_companion() {
                // Runtime keeps the resident pet/dog visible as a settled
                // still under Reduced Motion; the specimen must show the same
                // companion identity even though its scripted trail is frozen.
                PreviewTrailCompanion::Pet(species) => pet_layer(species),
                // Ordinary flying-kitty episodes are motion and remain
                // suppressed under Reduced Motion, exactly like runtime.
                PreviewTrailCompanion::FlyingKitty => (!spec.reduced_motion)
                    .then(|| kitty_layer(&spec.post_fx.kitty_asset))
                    .flatten(),
                PreviewTrailCompanion::None => None,
            }
        };
        if let Some((sprites, atlas)) = companion {
            input.free_sprites = sprites;
            input.free_atlas = Some(atlas);
        }
    }

    let input = Arc::new(input);
    LAST_SPECIMEN.with_borrow_mut(|slot| *slot = Some((fingerprint, Arc::clone(&input))));
    (input, fingerprint)
}

fn kitty_layer(
    source: &crate::app_config::KittySpriteAsset,
) -> Option<(
    Vec<aterm_core::render::FreeSprite>,
    Arc<aterm_core::render::SceneAtlas>,
)> {
    use std::sync::OnceLock;

    use aterm_effects::word_decorations::{
        EffectGeom, KittyCursorFrame, KittySpriteSource, WordDecorations,
    };

    static BUILT_IN: OnceLock<(
        Vec<aterm_core::render::FreeSprite>,
        Arc<aterm_core::render::SceneAtlas>,
    )> = OnceLock::new();
    let render = |decorations: &mut WordDecorations| {
        let mut sprites = Vec::new();
        let _ = decorations.kitty_cursor(
            KittyCursorFrame {
                geom: EffectGeom {
                    cell_w: 8,
                    cell_h: 18,
                    rows: 5,
                    cols: 32,
                },
                cursor: (1, 1),
                look: aterm_effects::kitty_registry::KittyLook::default(),
                colors: aterm_effects::cat_baker::CatColorKey::default(),
                bob: 0.0,
                alpha: 255,
                pose: aterm_effects::kitty_cursor::CatPose::STILL,
                // The settings preview is a still: no celebration, no notes.
                sing: 0.0,
                notes: [None; aterm_effects::kitty_sing::MAX_NOTES],
            },
            &mut sprites,
        );
        let atlas = decorations
            .free_atlas()
            .expect("CatBaker publishes an atlas after a bounded Nyan bake");
        (sprites, atlas)
    };
    match source {
        crate::app_config::KittySpriteAsset::BuiltIn => BUILT_IN
            .get_or_init(|| render(&mut WordDecorations::default()))
            .clone()
            .into(),
        crate::app_config::KittySpriteAsset::Ready { w, h, rgba, fp, .. } => {
            let mut decorations = WordDecorations::default();
            decorations.set_kitty_sprite_source(KittySpriteSource::Custom {
                source_fp: *fp,
                w: *w,
                h: *h,
                rgba: Arc::clone(rgba),
            });
            Some(render(&mut decorations))
        }
        crate::app_config::KittySpriteAsset::Invalid { .. } => None,
    }
}

fn pet_layer(
    species: aterm_effects::kitty_pet::PetSpecies,
) -> Option<(
    Vec<aterm_core::render::FreeSprite>,
    Arc<aterm_core::render::SceneAtlas>,
)> {
    use std::sync::OnceLock;

    use aterm_effects::kitty_pet::{PET_DEPARTURES_MAX, PET_MOTES_MAX, PetAction, PetFrame};
    use aterm_effects::word_decorations::{EffectGeom, PetCursorFrame, WordDecorations};

    type Layer = Option<(
        Vec<aterm_core::render::FreeSprite>,
        Arc<aterm_core::render::SceneAtlas>,
    )>;
    static CAT: OnceLock<Layer> = OnceLock::new();
    static DOG: OnceLock<Layer> = OnceLock::new();

    let render = || {
        let mut decorations = WordDecorations::default();
        let mut sprites = Vec::new();
        let pose = species.skin(aterm_effects::pet_glyphs_gen::PetGlyphId::PetSit);
        let _ = decorations.pet_cursor(
            PetCursorFrame {
                geom: EffectGeom {
                    cell_w: 8,
                    cell_h: 18,
                    rows: 5,
                    cols: 32,
                },
                colors: aterm_effects::cat_baker::CatColorKey::default(),
                coat: 0,
                iris: 0,
                pet: PetFrame {
                    alpha: 255,
                    // A static preview: no arrival ramp is in flight, so the
                    // lane byte and the body byte are the same presence.
                    lane_alpha: 255,
                    action: PetAction::Sit,
                    pose,
                    col: 1.0,
                    row: 1.0,
                    lift: 0.0,
                    facing_left: false,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    purr: 0.0,
                    under_ink: false,
                    motes: [None; PET_MOTES_MAX],
                    departures: [None; PET_DEPARTURES_MAX],
                },
            },
            &mut sprites,
        );
        let atlas = decorations.free_atlas()?;
        (!sprites.is_empty()).then_some((sprites, atlas))
    };

    match species {
        aterm_effects::kitty_pet::PetSpecies::Cat => CAT.get_or_init(render).clone(),
        aterm_effects::kitty_pet::PetSpecies::Dog => DOG.get_or_init(render).clone(),
    }
}

fn paint_portable_post_fx(
    prims: &mut Vec<DrawPrim>,
    grid: PreviewGrid,
    spec: &SettingsPreviewSpec,
    cursor: (u16, u16),
    theme: Theme,
) {
    if spec.reduced_motion || !spec.cursor.trail_enabled {
        return;
    }
    let cx = grid.rect.x + f32::from(cursor.1) * grid.cell_w as f32 + grid.cell_w as f32 * 0.5;
    let cy = grid.rect.y + f32::from(cursor.0) * grid.cell_h as f32 + grid.cell_h as f32 * 0.5;
    let luma = aterm_render::hdr::packed_luma(theme.bg);
    let sdr_budget = aterm_render::hdr::sdr_glow_budget(luma, spec.post_fx.sdr_boost);
    let hdr_tone = if spec.post_fx.hdr_glow { 0.18 } else { 0.0 };
    let strength = if spec.post_fx.bloom {
        spec.post_fx.bloom_strength / 3.0
    } else {
        0.0
    };
    let alpha = ((sdr_budget + hdr_tone) * strength * 255.0)
        .round()
        .clamp(0.0, 150.0) as u8;
    if alpha > 0 {
        let radius = grid.cell_h as f32 * (0.22 + spec.post_fx.bloom_radius * 0.12);
        for (scale, coverage) in [(1.0_f32, 0.35_f32), (0.62, 0.62), (0.3, 1.0)] {
            let r = radius * scale;
            prims.push(DrawPrim::AdditiveRect {
                x: cx - r,
                y: cy - r,
                w: r * 2.0,
                h: r * 2.0,
                premul: aterm_render::premul_rgb(
                    theme.cursor,
                    (f32::from(alpha) * coverage).round() as u8,
                ),
            });
        }
    }
    if spec.post_fx.fire_shimmer && spec.cursor.trail_style == PreviewTrailStyle::Fire {
        let wobble = ((spec.phase_ms / 52) % 5) as f32 - 2.0;
        prims.push(DrawPrim::AdditiveRect {
            x: cx - grid.cell_w as f32 * 0.8 + wobble,
            y: cy - grid.cell_h as f32 * 0.9,
            w: grid.cell_w as f32 * 1.6,
            h: 2.0,
            premul: aterm_render::premul_rgb(0x00FF_8A24, 80),
        });
    }
}

/// The borrowing core of [`normalized_title_format`], so the per-frame paint
/// identity can fold the same value in without allocating it and the two can
/// never drift apart.
fn normalized_title_format_str(value: &str) -> &str {
    match value.trim() {
        known @ ("title" | "description" | "title-description" | "description-title") => known,
        _ => "title-description",
    }
}

fn normalized_title_format(value: &str) -> String {
    normalized_title_format_str(value).to_string()
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

fn preview_title(title: &str, description: &str, format: &str, separator: &str) -> String {
    if title.is_empty() && description.is_empty() {
        return "aterm".to_string();
    }
    if title.is_empty() {
        return description.to_string();
    }
    if description.is_empty() || title == description {
        return title.to_string();
    }
    match format {
        "title" => title.to_string(),
        "description" => description.to_string(),
        "description-title" => format!("{description}{separator}{title}"),
        _ => format!("{title}{separator}{description}"),
    }
}

fn paint_window_tabs(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    spec: &SettingsPreviewSpec,
    roles: Roles,
    terminal_theme: Theme,
) {
    // Static, explicitly-authored examples: this demonstrates composition and
    // precedence without pretending a provider is connected. The first session
    // owns durable Description; the second has no authored Description, so its
    // generated Activity appears only while generation is enabled.
    let stable_title = "release";
    let authored_description = "shipping";
    let generated_activity = if spec.window_tabs.generate_activity {
        "running tests"
    } else {
        ""
    };
    let window_identity = preview_title(
        stable_title,
        authored_description,
        &spec.window_tabs.window_title_format,
        " — ",
    );
    let authored_tab = preview_title(
        stable_title,
        authored_description,
        &spec.window_tabs.tab_title_format,
        " · ",
    );
    let activity_tab = preview_title(
        "tests",
        generated_activity,
        &spec.window_tabs.tab_title_format,
        " · ",
    );
    let title_h = 19.0;
    let light = window_sample_light(spec, terminal_theme);
    let title_fill = if light {
        [0xE9, 0xEA, 0xEE]
    } else {
        [0x20, 0x22, 0x2A]
    };
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: title_h,
        radius: 0.0,
        fill: rgba(title_fill, 255),
        blur: false,
    });
    let title_inset = paint_platform_window_controls(prims, rect.x, rect.y, title_h, light);
    let caption = TypeStep::Caption.px(13.75);
    prims.push(text_prim(
        rect.x + title_inset,
        crate::tray_raster::row_baseline(rect.y, title_h, caption.get()),
        format!("Settings · {window_identity}"),
        caption,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(window_chrome_text_color(light), 255),
    ));
    if spec.window_tabs.show_build_badge {
        prims.push(DrawPrim::Panel {
            x: rect.right() - 42.0,
            y: rect.y + 4.0,
            w: 36.0,
            h: 11.0,
            radius: 5.5,
            fill: rgba(roles.accent, 210),
            blur: false,
        });
    }
    let strip_rows = spec.window_tabs.tab_strip_rows.min(4);
    let strip_h = 13.0;
    for row in 0..strip_rows {
        let y = rect.y + title_h + row as f32 * strip_h;
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y,
            w: rect.width,
            h: strip_h,
            radius: 0.0,
            fill: rgba(roles.elevated, 245),
            blur: false,
        });
        // Authored Description and generated Activity occupy separate examples,
        // both composed through the selected tab format.
        prims.push(text_prim(
            rect.x + 7.0,
            crate::tray_raster::row_baseline(y, strip_h, caption.get()),
            authored_tab.clone(),
            caption,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(roles.text_primary, 255),
        ));
        let settings_x = rect.x + rect.width * 0.54;
        prims.push(DrawPrim::Dot {
            cx: settings_x + 3.0,
            cy: y + strip_h * 0.5,
            r: 2.0,
            color: rgba(roles.accent, 255),
            breathe: false,
        });
        prims.push(text_prim(
            settings_x + 8.0,
            crate::tray_raster::row_baseline(y, strip_h, caption.get()),
            activity_tab.clone(),
            caption,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(roles.text_primary, 255),
        ));
    }
    let content_y = rect.y + title_h + strip_rows as f32 * strip_h;
    let content_h = (rect.bottom() - content_y).max(1.0);
    let fact_px = TypeStep::Caption.px(12.0);
    for (index, (label, value, color)) in [
        ("TITLE", stable_title, roles.text_primary),
        ("DESCRIPTION", authored_description, roles.text_secondary),
        (
            "ACTIVITY FALLBACK",
            if spec.window_tabs.generate_activity {
                "running tests"
            } else {
                "off"
            },
            roles.accent,
        ),
        (
            "TAB FORMAT",
            spec.window_tabs.tab_title_format.as_str(),
            roles.text_tertiary,
        ),
        (
            "WINDOW FORMAT",
            spec.window_tabs.window_title_format.as_str(),
            roles.text_tertiary,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        prims.push(text_prim(
            rect.x + 9.0,
            content_y + 16.0 + index as f32 * 17.0,
            format!("{label}  ·  {value}"),
            fact_px,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(color, 255),
        ));
    }
    let vertical = (spec.window_tabs.columns / 20).clamp(3, 12);
    let horizontal = (spec.window_tabs.lines / 6).clamp(2, 10);
    for column in 1..vertical {
        let x = rect.x + rect.width * column as f32 / vertical as f32;
        prims.push(DrawPrim::Line {
            x1: x,
            y1: content_y,
            x2: x,
            y2: content_y + content_h,
            width: 0.5,
            color: rgba(roles.separator, 90),
        });
    }
    for row in 1..horizontal {
        let y = content_y + content_h * row as f32 / horizontal as f32;
        prims.push(DrawPrim::Line {
            x1: rect.x,
            y1: y,
            x2: rect.right(),
            y2: y,
            width: 0.5,
            color: rgba(roles.separator, 90),
        });
    }
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

#[derive(Clone, Copy, Debug)]
struct PreviewGrid {
    rect: LogicalRect,
    font_px: f32,
    cell_w: usize,
    cell_h: usize,
    baseline: i32,
    rows: usize,
    cols: usize,
}

impl PreviewGrid {
    fn new(terminal: LogicalRect, font_px: f32, line_height: f32, baseline_adjust: i32) -> Self {
        Self::new_for_candidate(
            terminal,
            font_px,
            line_height,
            baseline_adjust,
            &SemanticFontCandidate::default(),
        )
    }

    fn new_for_candidate(
        terminal: LogicalRect,
        font_px: f32,
        line_height: f32,
        baseline_adjust: i32,
        _candidate: &SemanticFontCandidate,
    ) -> Self {
        let pad_x = 10.0;
        let pad_y = 8.0;
        let rect = LogicalRect::new(
            terminal.x + pad_x,
            terminal.y + pad_y,
            (terminal.width - 2.0 * pad_x).max(1.0),
            (terminal.height - 2.0 * pad_y).max(1.0),
        );
        let cell_w = (font_px * 0.62).round().clamp(4.0, 48.0) as usize;
        let cell_h = (font_px * 1.42 * line_height).round().clamp(11.0, 72.0) as usize;
        let baseline =
            ((cell_h as f32 - font_px) * 0.5 + font_px * 0.8).round() as i32 + baseline_adjust;
        let rows = ((rect.height / cell_h as f32).floor() as usize).clamp(1, 8);
        let cols = ((rect.width / cell_w as f32).floor() as usize).clamp(1, 96);
        Self {
            rect,
            font_px,
            cell_w,
            cell_h,
            baseline,
            rows,
            cols,
        }
    }

    fn start_cursor(self) -> (u16, u16) {
        (
            u16::try_from(self.rows.saturating_sub(1).min(3)).unwrap_or(0),
            u16::try_from(self.cols.saturating_sub(1).min(4)).unwrap_or(0),
        )
    }
}

#[derive(Default)]
struct PreviewEffects {
    cursor: (u16, u16),
    trail_color: u32,
    trail: Vec<TrailCell>,
    glow: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
    halos: Vec<RainHalo>,
    patches: Vec<FirePatch>,
}

fn scripted_cursor(grid: PreviewGrid, step: usize) -> (u16, u16) {
    let row = grid.rows.saturating_sub(1).min(3);
    let start = grid.cols.saturating_sub(1).min(3);
    let span = grid.cols.saturating_sub(start + 2).max(1);
    let period = span.saturating_mul(2).max(1);
    let phase = step % period;
    let offset = if phase <= span { phase } else { period - phase };
    (
        u16::try_from(row).unwrap_or(u16::MAX),
        u16::try_from(start.saturating_add(offset)).unwrap_or(u16::MAX),
    )
}

fn paint_selection(prims: &mut Vec<DrawPrim>, grid: PreviewGrid, theme: Theme) {
    if grid.rows < 2 || grid.cols < 8 {
        return;
    }
    prims.push(DrawPrim::Panel {
        x: grid.rect.x + 7.0 * grid.cell_w as f32,
        y: grid.rect.y + grid.cell_h as f32,
        w: (grid.cols.saturating_sub(9).min(12) * grid.cell_w) as f32,
        h: grid.cell_h as f32,
        radius: 2.0,
        fill: rgba(crate::settings::u32_rgb(theme.selection), 255),
        blur: false,
    });
}

fn paint_trail(
    prims: &mut Vec<DrawPrim>,
    grid: PreviewGrid,
    trail: &[TrailCell],
    trail_color: u32,
    background: u32,
) {
    for cell in trail {
        if cell.row >= grid.rows || cell.col >= grid.cols {
            continue;
        }
        let color = aterm_render::blend_rgb(background, trail_color, cell.alpha);
        prims.push(DrawPrim::Panel {
            x: grid.rect.x + cell.col as f32 * grid.cell_w as f32,
            y: grid.rect.y + cell.row as f32 * grid.cell_h as f32,
            w: grid.cell_w as f32,
            h: grid.cell_h as f32,
            radius: 0.0,
            fill: rgba(crate::settings::u32_rgb(color), 255),
            blur: false,
        });
    }
}

fn paint_effect_under(prims: &mut Vec<DrawPrim>, grid: PreviewGrid, effects: &PreviewEffects) {
    for quad in &effects.under {
        push_glow_quad(prims, grid, *quad);
    }
    for mode in [aterm_render::FireMode::Add, aterm_render::FireMode::Over] {
        for patch in effects.patches.iter().filter(|patch| patch.mode == mode) {
            prims.push(DrawPrim::EffectFire {
                patch: *patch,
                offset_x: grid.rect.x,
                offset_y: grid.rect.y,
            });
        }
    }
}

fn paint_effect_over(prims: &mut Vec<DrawPrim>, grid: PreviewGrid, effects: &PreviewEffects) {
    for quad in &effects.glow {
        push_glow_quad(prims, grid, *quad);
    }
    // CursorGlow emits Add before Over, matching aterm-render's stream order.
    for mode in [aterm_render::HaloMode::Add, aterm_render::HaloMode::Over] {
        for halo in effects.halos.iter().filter(|halo| halo.mode == mode) {
            prims.push(DrawPrim::EffectHalo {
                halo: *halo,
                offset_x: grid.rect.x,
                offset_y: grid.rect.y,
            });
        }
    }
}

fn push_glow_quad(prims: &mut Vec<DrawPrim>, grid: PreviewGrid, quad: GlowQuad) {
    prims.push(DrawPrim::AdditiveRect {
        x: grid.rect.x + f32::from(quad.x),
        y: grid.rect.y + f32::from(quad.y),
        w: f32::from(quad.w),
        h: f32::from(quad.h),
        premul: quad.color,
    });
}

fn paint_cursor(
    prims: &mut Vec<DrawPrim>,
    grid: PreviewGrid,
    spec: &SettingsPreviewSpec,
    cursor: (u16, u16),
    theme: Theme,
) {
    if spec.cursor.style == PreviewCursorStyle::Hidden {
        return;
    }
    let visible = spec.cursor_visible();
    if !visible || cursor.0 as usize >= grid.rows || cursor.1 as usize >= grid.cols {
        return;
    }
    let x = grid.rect.x + f32::from(cursor.1) * grid.cell_w as f32;
    let y = grid.rect.y + f32::from(cursor.0) * grid.cell_h as f32;
    let color = rgba(crate::settings::u32_rgb(theme.cursor), 238);
    let (x, y, width, height, radius) = match spec.cursor.style {
        PreviewCursorStyle::Block => (
            x,
            y + 1.0,
            grid.cell_w as f32,
            grid.cell_h as f32 - 2.0,
            2.0,
        ),
        PreviewCursorStyle::Bar => (x, y + 1.0, 2.0, grid.cell_h as f32 - 2.0, 1.0),
        PreviewCursorStyle::Underline => (
            x,
            y + grid.cell_h.saturating_sub(3) as f32,
            grid.cell_w as f32,
            2.0,
            1.0,
        ),
        PreviewCursorStyle::Hidden => return,
    };
    prims.push(DrawPrim::Panel {
        x,
        y,
        w: width.max(1.0),
        h: height.max(1.0),
        radius,
        fill: color,
        blur: false,
    });
}

#[derive(Clone, Copy)]
enum SampleTone {
    Primary,
    Muted,
    Accent,
    Success,
}

fn sample_lines(scene: PreviewScene) -> &'static [(&'static str, SampleTone)] {
    match scene {
        PreviewScene::Appearance => &[
            ("~/aterm  main", SampleTone::Muted),
            ("$ cargo test -p aterm-gui", SampleTone::Primary),
            ("running semantic renderer checks", SampleTone::Accent),
            ("1391 passed; 0 failed", SampleTone::Success),
            ("$ ", SampleTone::Primary),
        ],
        PreviewScene::Typography => &[
            ("Aa Bb 012345  λ π √2 ✓  → aterm", SampleTone::Primary),
            ("你好世界 · 日本語 · 한글 · 🚀 😀 🐈‍⬛", SampleTone::Accent),
            ("Ligatures: != == => ->  ⌘ ⇧", SampleTone::Primary),
            ("Combining: cafe\u{301}  naïve  résumé", SampleTone::Success),
        ],
        PreviewScene::CursorMotion => &[
            ("aterm effects are real renderer output", SampleTone::Muted),
            ("$ cargo test --workspace", SampleTone::Primary),
            ("motion stays readable beneath the wake", SampleTone::Accent),
            ("type here: semantic cursor runway", SampleTone::Primary),
            ("ready", SampleTone::Success),
        ],
        PreviewScene::WindowTabs => &[],
    }
}

/// Return the longest complete-grapheme prefix that fits a terminal row.
/// Wide CJK/emoji clusters consume two columns and ZWJ/combining sequences are
/// never split at the preview clip edge.
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

fn bounded_badge(value: &str, max_graphemes: usize) -> String {
    let mut end = 0;
    for (count, (offset, grapheme)) in value.grapheme_indices().enumerate() {
        if count == max_graphemes {
            return format!("{}…", &value[..end]);
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
    format!("{}…", &value[..end])
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::native_ui::{GroupSpec, SemanticRole, UiTree};

    const VIEWPORT: LogicalRect = LogicalRect::new(0.0, 0.0, 480.0, 210.0);
    const NORD: PreviewTerminalTheme =
        PreviewTerminalTheme::new(0xD8_DEE9, 0x2E_3440, 0x88_C0D0, 0x43_4C5E);

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

    fn preview_text(spec: SettingsPreviewSpec) -> Vec<String> {
        compiled(spec)
            .tray(Theme::default(), 13.0)
            .prims
            .into_iter()
            .filter_map(|primitive| match primitive {
                DrawPrim::Text { s, .. } => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn smart_title_preview_distinguishes_identity_description_and_activity_formats() {
        let base = SettingsPreviewSpec::window_tabs(WindowTabsPreviewSpec::default(), 14.0);
        assert_eq!(base.animation(), PreviewAnimation::None);
        let text = preview_text(base.clone());
        for expected in [
            "Settings · release — shipping",
            "release · shipping",
            "tests · running tests",
            "TITLE  ·  release",
            "DESCRIPTION  ·  shipping",
            "ACTIVITY FALLBACK  ·  running tests",
            "TAB FORMAT  ·  title-description",
            "WINDOW FORMAT  ·  title-description",
        ] {
            assert!(
                text.iter().any(|line| line == expected),
                "missing truthful Smart Titles example {expected:?}: {text:?}"
            );
        }
        let audit = base.audit_value();
        assert!(audit.contains("stable Title `release`"));
        assert!(audit.contains("authored Description `shipping`"));
        assert!(audit.contains("generated Activity fallback `running tests` is shown"));
        assert!(audit.contains("no live provider-health claim"));

        let description_first = SettingsPreviewSpec::window_tabs(
            WindowTabsPreviewSpec {
                tab_title_format: "description-title".to_string(),
                window_title_format: "description-title".to_string(),
                ..WindowTabsPreviewSpec::default()
            },
            14.0,
        );
        let description_first_text = preview_text(description_first.clone());
        assert!(
            description_first_text
                .iter()
                .any(|line| line == "Settings · shipping — release")
        );
        assert!(
            description_first_text
                .iter()
                .any(|line| line == "shipping · release")
        );
        assert!(
            description_first_text
                .iter()
                .any(|line| line == "running tests · tests")
        );
        assert!(differences(&pixels(base), &pixels(description_first)) > 40);

        let activity_off = SettingsPreviewSpec::window_tabs(
            WindowTabsPreviewSpec {
                generate_activity: false,
                ..WindowTabsPreviewSpec::default()
            },
            14.0,
        );
        let activity_off_text = preview_text(activity_off.clone());
        assert!(
            activity_off_text
                .iter()
                .any(|line| line == "release · shipping"),
            "authored Description is unaffected by the Activity switch"
        );
        assert!(activity_off_text.iter().any(|line| line == "tests"));
        assert!(
            activity_off_text
                .iter()
                .any(|line| line == "ACTIVITY FALLBACK  ·  off")
        );
        assert!(
            activity_off
                .semantic_value()
                .contains("fallback `running tests` is off")
        );
        assert_eq!(activity_off.animation(), PreviewAnimation::None);
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
        assert!(moving.semantic_value().contains("Reduce Motion"));
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
        assert!(value.contains("foreground #D8DEE9"));
        assert!(value.contains("background #2E3440"));

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

        let tabs_light = SettingsPreviewSpec::window_tabs(WindowTabsPreviewSpec::default(), 14.0)
            .with_terminal_theme(NORD)
            .with_system_dark(false);
        let tabs_dark = tabs_light.clone().with_system_dark(true);
        assert!(differences(&pixels(tabs_light), &pixels(tabs_dark)) > 40);

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
        const PAPER: PreviewTerminalTheme =
            PreviewTerminalTheme::new(0x33_3333, 0xFA_FAFA, 0x35_84E4, 0xD0_D7DE);
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
        let light_palette = dark_palette.clone().with_terminal_theme(PAPER);
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
    fn window_preview_controls_match_the_compiled_platform() {
        let mut prims = Vec::new();
        let rect = LogicalRect::new(4.0, 6.0, 240.0, 120.0);
        paint_window_tabs(
            &mut prims,
            rect,
            &SettingsPreviewSpec::window_tabs(WindowTabsPreviewSpec::default(), 14.0),
            Roles::from_theme(Theme::default()),
            Theme::default(),
        );
        let title_x = prims.iter().find_map(|primitive| match primitive {
            DrawPrim::Text { x, s, .. } if s.starts_with("Settings · ") => Some(*x),
            _ => None,
        });
        #[cfg(target_os = "macos")]
        {
            assert_eq!(title_x, Some(rect.x + 43.0));
            assert_eq!(
                prims
                    .iter()
                    .filter(|primitive| matches!(
                        primitive,
                        DrawPrim::Dot { cy, .. } if *cy <= rect.y + 19.0
                    ))
                    .count(),
                3
            );
            assert!(
                !prims
                    .iter()
                    .any(|primitive| matches!(primitive, DrawPrim::Stroke { .. }))
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(title_x, Some(rect.x + 22.0));
            assert_eq!(
                prims
                    .iter()
                    .filter(|primitive| matches!(primitive, DrawPrim::Stroke { .. }))
                    .count(),
                1
            );
            assert!(!prims.iter().any(|primitive| matches!(
                primitive,
                DrawPrim::Dot { cy, .. } if *cy <= rect.y + 19.0
            )));
        }

        // The terminal/native Settings palette can be dark while window chrome
        // is explicitly Light (and vice versa). Caption ink therefore belongs
        // to the titlebar, not `roles.text_primary`; pin both paint identity and
        // WCAG-normal-text contrast against the exact fills used above.
        for (window_theme, light, fill) in [
            ("light", true, [0xE9, 0xEA, 0xEE]),
            ("dark", false, [0x20, 0x22, 0x2A]),
        ] {
            let mut prims = Vec::new();
            let spec = SettingsPreviewSpec::window_tabs(WindowTabsPreviewSpec::default(), 14.0)
                .with_appearance(AppearancePreviewSpec {
                    window_theme: window_theme.to_string(),
                    ..AppearancePreviewSpec::default()
                });
            paint_window_tabs(
                &mut prims,
                rect,
                &spec,
                Roles::from_theme(Theme::default()),
                Theme::default(),
            );
            let caption = prims.iter().find_map(|primitive| match primitive {
                DrawPrim::Text { s, color, .. } if s.starts_with("Settings · ") => Some(*color),
                _ => None,
            });
            let ink = window_chrome_text_color(light);
            assert_eq!(caption, Some(rgba(ink, 255)), "{window_theme} caption ink");
            assert!(
                test_contrast_ratio(ink, fill) >= 4.5,
                "{window_theme} caption must contrast with its own titlebar"
            );
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
        assert_eq!(
            swatches.len(),
            8,
            "the complete bright ANSI row remains visible"
        );
        for (index, (x, y, width, fill)) in swatches.iter().enumerate() {
            assert_eq!(
                *fill,
                rgba(crate::settings::u32_rgb(candidate.ansi[index + 8]), 255)
            );
            assert!(*x >= rect.x && *x + *width <= rect.right() + 0.01);
            assert!(*y >= rect.y && *y + 6.0 <= rect.bottom() + 0.01);
        }
    }

    #[test]
    fn small_top_theme_card_keeps_the_real_specimen_above_its_palette_band() {
        // Install and capture the same prepared renderer source a live native
        // Settings view receives. The fallback label belongs only to the
        // unavailable first frame and must not paint over a real specimen.
        let _ = compiled(SettingsPreviewSpec::appearance(14.0));
        let prepared = crate::tray_raster::prepare_semantic_font(
            &crate::widget::SemanticFontCandidate::default(),
        );
        assert!(prepared.is_ready());
        let spec = SettingsPreviewSpec::appearance(24.0).with_prepared_font(prepared);
        let rect = LogicalRect::new(0.0, 0.0, 360.0, 132.0);
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            rect,
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );

        let specimen = prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.as_ref()),
                _ => None,
            })
            .expect("real terminal specimen");
        assert_eq!(specimen.input.cells.len(), APPEARANCE_SPECIMEN_ROWS);
        let terminal = LogicalRect::new(10.0, 32.0, 340.0, 90.0);
        let specimen_clip = appearance_specimen_clip(terminal);
        let projected_height = specimen.font_px
            * specimen.line_height.max(0.5)
            * APPEARANCE_RENDERER_LINE_ALLOWANCE
            * APPEARANCE_SPECIMEN_ROWS as f32;
        assert!(
            projected_height <= specimen_clip.height + 0.01,
            "{projected_height} > {}",
            specimen_clip.height
        );
        let terminal_text = specimen
            .input
            .cells
            .iter()
            .flatten()
            .map(|cell| cell.ch)
            .collect::<String>();
        assert!(terminal_text.contains("gypq_j underline"));

        let swatch_y = appearance_swatch_y(terminal);
        assert!(specimen_clip.bottom() + APPEARANCE_SPECIMEN_SWATCH_GAP <= swatch_y + 0.01);
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
        assert!(
            prims
                .iter()
                .filter(|primitive| matches!(
                    primitive,
                    DrawPrim::Panel { y, h, .. }
                        if (*y - swatch_y).abs() < 0.01
                            && (*h - APPEARANCE_SWATCH_HEIGHT).abs() < 0.01
                ))
                .count()
                >= 8
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
        assert_eq!(specimen.input.cursor_col, 1);
        assert!(specimen.input.selection.has_selection());

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
            ..CursorPreviewSpec::default()
        })
        .with_phase(900)
        .normalized();
        assert_eq!(spec.cursor.duration_ms, MAX_DURATION_MS);
        assert_eq!(spec.cursor.length, MAX_LENGTH);
        assert_eq!(spec.cursor.intensity, 0.0);
        assert_eq!(spec.cursor.radius, 0.0);

        let grid = PreviewGrid::new(
            LogicalRect::new(0.0, 0.0, 420.0, 150.0),
            spec.font_px,
            spec.line_height,
            spec.baseline_adjust,
        );
        let output = spec.effects(grid, Theme::default());
        assert!(output.glow.is_empty());
        assert!(output.trail.is_empty());
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

        let (cat_sprites, cat_atlas) = pet_layer(PetSpecies::Cat).expect("cat preview layer");
        let (dog_sprites, dog_atlas) = pet_layer(PetSpecies::Dog).expect("dog preview layer");
        assert!(!cat_sprites.is_empty() && !cat_atlas.rgba.is_empty());
        assert!(!dog_sprites.is_empty() && !dog_atlas.rgba.is_empty());
        assert_ne!(
            cat_atlas.rgba.as_slice(),
            dog_atlas.rgba.as_slice(),
            "species must change pixels"
        );
        let fingerprint = |rgba: &[u8]| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            rgba.hash(&mut hash);
            hash.finish()
        };
        assert_ne!(fingerprint(&cat_atlas.rgba), fingerprint(&dog_atlas.rgba));
    }

    #[test]
    fn reduced_motion_preview_keeps_resident_pets_but_suppresses_flight() {
        let reduced_input = |raw: &'static str| {
            build_terminal_specimen_input(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_reduced_motion(true),
                Theme::default(),
            )
            .0
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

        let (live_flying, _) = build_terminal_specimen_input(
            &SettingsPreviewSpec::cursor(rainbow_cursor("rainbow kitty flying")),
            Theme::default(),
        );
        assert!(
            !live_flying.free_sprites.is_empty() && live_flying.free_atlas.is_some(),
            "negative control: the selected flying companion is drawable outside Reduced Motion"
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

        let grid = PreviewGrid::new(
            LogicalRect::new(0.0, 0.0, 420.0, 150.0),
            synthwave_spec.font_px,
            synthwave_spec.line_height,
            synthwave_spec.baseline_adjust,
        );
        let output = synthwave_spec.effects(grid, Theme::default());
        assert!(
            !output.glow.is_empty()
                || !output.under.is_empty()
                || !output.halos.is_empty()
                || !output.patches.is_empty(),
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

        let prims = compiled(custom.clone()).tray(Theme::default(), 13.0).prims;
        let specimen = prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec),
                _ => None,
            })
            .expect("terminal specimen");
        assert!(!specimen.input.free_sprites.is_empty());
        assert!(specimen.input.free_atlas.is_some());
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

        let grid = PreviewGrid::new(
            LogicalRect::new(0.0, 0.0, 420.0, 150.0),
            unavailable.font_px,
            unavailable.line_height,
            unavailable.baseline_adjust,
        );
        let output = unavailable.effects(grid, Theme::default());
        assert!(output.trail.is_empty());
        assert!(output.glow.is_empty());
        assert!(output.under.is_empty());
        assert!(output.halos.is_empty());
        assert!(output.patches.is_empty());
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
        let grid = PreviewGrid::new(
            LogicalRect::new(0.0, 0.0, 420.0, 150.0),
            spec.font_px,
            spec.line_height,
            spec.baseline_adjust,
        );
        assert!(!spec.effects(grid, Theme::default()).glow.is_empty());
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
            let grid = PreviewGrid::new(
                LogicalRect::new(0.0, 0.0, 420.0, 150.0),
                spec.font_px,
                spec.line_height,
                spec.baseline_adjust,
            );
            let output = spec.effects(grid, Theme::default());
            assert!(output.glow.is_empty(), "{raw}");
            assert!(output.trail.is_empty(), "{raw}");
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
            SettingsPreviewSpec::window_tabs(WindowTabsPreviewSpec::default(), 14.0),
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

    #[test]
    fn settled_default_font_is_presented_as_the_live_host_not_an_unavailable_choice() {
        let mut spec = SettingsPreviewSpec::typography(14.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default");
        // This is the exact settled status emitted for the host candidate by
        // `SemanticFontResolution::Host`.  Keep the presentation assertion in
        // this module, beside the classifier that owns the warning capsule.
        spec.font_status = "committed renderer font cascade ready".to_string();

        assert_eq!(
            spec.font_candidate_pill(),
            Some(("Live specimen · Regular: default".to_string(), false))
        );
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
        let underline = shape(PreviewCursorStyle::Underline);
        assert!(differences(&block, &bar) > 40);
        assert!(differences(&bar, &underline) > 20);
        assert!(differences(&block, &underline) > 40);
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

    /// A libtest worker is reusable. Pin the second half of the fixture
    /// contract: if a different test installs another settled cascade on that
    /// worker, the next preview paint notices the generation change and
    /// restores this module's exact embedded-font fixture.
    #[test]
    fn fixture_repairs_reused_worker_font_context() {
        let spec = SettingsPreviewSpec::appearance(14.0).with_phase(100);
        let expected = pixels(spec.clone());
        let fixture_epoch = crate::tray_raster::chrome_font_epoch_for_test();

        let mut other = aterm_render::Renderer::from_bytes(
            include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf"),
            14.0,
            Theme::default(),
        )
        .expect("alternate semantic renderer");
        other.set_runtime_font_discovery(false);
        other.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
        let alternate_epoch = crate::tray_raster::install_settled_chrome_fonts_for_test(other);
        assert_ne!(alternate_epoch, fixture_epoch, "the worker context changed");

        let repaired = pixels(spec);
        assert_ne!(
            crate::tray_raster::chrome_font_epoch_for_test(),
            alternate_epoch,
            "compiled() must reinstall its fixture after worker reuse"
        );
        assert_eq!(
            differences(&expected, &repaired),
            0,
            "worker reuse must not change preview pixels"
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
        let hidden = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style: PreviewCursorStyle::Hidden,
            blink: true,
            trail_enabled: false,
            trail_style: PreviewTrailStyle::Off,
            ..CursorPreviewSpec::default()
        });
        assert_eq!(hidden.animation(), PreviewAnimation::None);

        let zero_intensity = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Phaser,
            intensity: 0.0,
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
}
