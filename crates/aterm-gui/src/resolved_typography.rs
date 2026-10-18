// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one typography resolver, sibling of [`crate::resolved_colors`]:
//! [`Config::resolved_typography`] picks the theme an appearance draws (by
//! the same selection as the colours, [`Config::theme_selection`]) and works
//! out which of the theme's type settings apply. [`ResolvedTypography::overlay`]
//! fills exactly those into a view of the config, so every existing reader of a
//! font key (`FontConfig`, `RenderKnobs`, `GlyphRasterKnobs`, `text_shaping`,
//! the variation requests, the family request) keeps reading a `Config` and
//! obeys `--font` > aterm.toml > theme > default without a change of its own.
//!
//! A theme preview (Settings Try, `ctl theme preview`, the palette) is colour
//! only (DTP §9.3): this resolver ignores the preview candidate on purpose.

use std::borrow::Cow;

use aterm_types::Appearance;
use aterm_types::scheme::{THEME_KEYS, ThemeKey, ThemeKeyGroup, ThemeTypography};

use crate::app_config::{Config, FontList, ThemeCatalog};

/// Who decides one type setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeOrigin {
    /// Nobody set it: aterm's default.
    Default,
    /// The theme's file.
    Theme,
    /// A key in aterm.toml.
    Config,
    /// A launch flag (`--font`).
    Flag,
}

impl TypeOrigin {
    /// The word `--show-config`, `ctl theme` and Settings use.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Theme => "theme",
            Self::Config => "config",
            Self::Flag => "flag",
        }
    }
}

/// One theme type setting a higher rung beat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypeShadow {
    /// The theme key (`font_family`).
    pub(crate) key: &'static str,
    /// Who beat it.
    pub(crate) origin: TypeOrigin,
    /// What beat it, as a person would look for it: the aterm.toml key
    /// (`font_family`, `display_font`) or the flag (`--font`).
    pub(crate) by: &'static str,
}

/// The typography one appearance draws: the theme's type settings and which
/// of them apply over the config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTypography {
    /// The theme drawn (the same id the colours report).
    pub(crate) id: String,
    /// The appearance it was resolved for.
    pub(crate) side: Appearance,
    /// What the theme file says.
    pub(crate) declared: ThemeTypography,
    /// The subset no config key or flag beats: what [`Self::overlay`] fills.
    pub(crate) applied: ThemeTypography,
    /// The theme's keys a config key or a flag beat, in table order.
    pub(crate) shadowed: Vec<TypeShadow>,
}

/// Whether `cfg` sets the typography key `key` itself (a whole value: an
/// empty list or a blank family counts). A non-finite `line_height` or
/// `font_size_scale` sets nothing: its resolver falls back as if it were
/// absent, so a theme's value fills it and the source says so.
pub(crate) fn config_sets(cfg: &Config, key: ThemeKey) -> bool {
    match key {
        ThemeKey::FontFamily => cfg.font_family.is_some(),
        ThemeKey::FontFamilyBold => cfg.font_family_bold.is_some(),
        ThemeKey::FontFamilyItalic => cfg.font_family_italic.is_some(),
        ThemeKey::FontFamilyBoldItalic => cfg.font_family_bold_italic.is_some(),
        ThemeKey::FallbackFonts => cfg.fallback_fonts.is_some(),
        ThemeKey::SymbolFont => cfg.symbol_font.is_some(),
        ThemeKey::EmojiFont => cfg.emoji_font.is_some(),
        ThemeKey::LineHeight => cfg.line_height.is_some_and(f32::is_finite),
        ThemeKey::Ligatures => cfg.ligatures.is_some(),
        ThemeKey::FontFeatures => cfg.font_features.is_some(),
        ThemeKey::FontSyntheticStyle => cfg.font_synthetic_style.is_some(),
        ThemeKey::FontWeight => cfg.font_weight.is_some(),
        ThemeKey::FontSizeScale => cfg.font_size_scale.is_some_and(f32::is_finite),
        _ => false,
    }
}

/// A thousandths value as the `f32` aterm.toml would hold for the same
/// decimal: the decimal's `f64`, narrowed. A theme's `1.15` and a config's
/// `1.15` are then the same bits.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the TOML f64 -> f32 narrowing, on purpose, so the bits match a config value"
)]
pub(crate) fn milli_f32(milli: u16) -> f32 {
    (f64::from(milli) / 1000.0) as f32
}

impl Config {
    /// The typography `appearance` draws against `themes`, for the launch's
    /// own `--font`. Ignores any theme preview on purpose: preview and Try are
    /// colour-only (DTP §9.3).
    pub(crate) fn resolved_typography(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
    ) -> ResolvedTypography {
        self.resolved_typography_with_flag(
            appearance,
            themes,
            crate::launch::flags().font_family.as_deref(),
        )
    }

    /// [`Self::resolved_typography`] with `flag` standing in for `--font`.
    pub(crate) fn resolved_typography_with_flag(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        flag: Option<&str>,
    ) -> ResolvedTypography {
        let selection = self.theme_selection(appearance, themes, None);
        let declared = selection.package.typography.clone();
        let mut applied = declared.clone();
        let mut shadowed = Vec::new();
        for spec in THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography && declared.is_set(spec.key))
        {
            let beaten = if spec.key == ThemeKey::FontFamily && flag.is_some() {
                Some((TypeOrigin::Flag, "--font"))
            } else if config_sets(self, spec.key) {
                Some((TypeOrigin::Config, spec.name))
            } else if spec.key == ThemeKey::FontFamily && self.font_family_request().is_some() {
                // `display_font` outranks any family (font_family_request).
                Some((TypeOrigin::Config, "display_font"))
            } else {
                None
            };
            if let Some((origin, by)) = beaten {
                applied.clear(spec.key);
                shadowed.push(TypeShadow {
                    key: spec.name,
                    origin,
                    by,
                });
            }
        }
        ResolvedTypography {
            id: selection.id,
            side: appearance,
            declared,
            applied,
            shadowed,
        }
    }
}

impl ResolvedTypography {
    /// No theme typography: what a config without a typography theme draws.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn none(appearance: Appearance) -> Self {
        Self {
            id: String::new(),
            side: appearance,
            declared: ThemeTypography::NONE,
            applied: ThemeTypography::NONE,
            shadowed: Vec::new(),
        }
    }

    /// The config every font reader should see: `cfg` with the applied theme
    /// settings filled into the keys it leaves unset (flag > config > theme >
    /// default). Borrowed, so byte-identical, when nothing applies.
    pub(crate) fn overlay<'a>(&self, cfg: &'a Config) -> Cow<'a, Config> {
        let t = &self.applied;
        if t.is_empty() {
            return Cow::Borrowed(cfg);
        }
        let mut view = cfg.clone();
        let fill = |slot: &mut Option<String>, value: &Option<String>| {
            if slot.is_none() {
                slot.clone_from(value);
            }
        };
        fill(&mut view.font_family, &t.font_family);
        fill(&mut view.font_family_bold, &t.font_family_bold);
        fill(&mut view.font_family_italic, &t.font_family_italic);
        fill(
            &mut view.font_family_bold_italic,
            &t.font_family_bold_italic,
        );
        fill(&mut view.symbol_font, &t.symbol_font);
        fill(&mut view.emoji_font, &t.emoji_font);
        if view.fallback_fonts.is_none() {
            view.fallback_fonts = t.fallback_fonts.clone().map(FontList);
        }
        if !view.line_height.is_some_and(f32::is_finite) && t.line_height_milli.is_some() {
            view.line_height = t.line_height_milli.map(milli_f32);
        }
        if view.ligatures.is_none() {
            view.ligatures = t.ligatures;
        }
        if view.font_features.is_none() {
            view.font_features.clone_from(&t.font_features);
        }
        if view.font_synthetic_style.is_none() {
            view.font_synthetic_style = t.font_synthetic_style;
        }
        if view.font_weight.is_none() {
            view.font_weight = t.font_weight.map(u32::from);
        }
        if !view.font_size_scale.is_some_and(f32::is_finite) && t.font_size_scale_milli.is_some() {
            view.font_size_scale = t.font_size_scale_milli.map(milli_f32);
        }
        Cow::Owned(view)
    }

    /// Who decides the typography key `key` (canonical spelling) in this
    /// resolution, for `cfg` (the authored config it was resolved from).
    pub(crate) fn origin(&self, key: &str, cfg: &Config) -> TypeOrigin {
        if let Some(shadow) = self.shadowed.iter().find(|shadow| shadow.key == key) {
            return shadow.origin;
        }
        let Some(spec) = THEME_KEYS.iter().find(|spec| spec.name == key) else {
            return TypeOrigin::Default;
        };
        if spec.key == ThemeKey::FontFamily && crate::launch::flags().font_family.is_some() {
            TypeOrigin::Flag
        } else if config_sets(cfg, spec.key) {
            TypeOrigin::Config
        } else if self.applied.is_set(spec.key) {
            TypeOrigin::Theme
        } else {
            TypeOrigin::Default
        }
    }

    /// The face-relevant fingerprint: two resolutions need no font generation
    /// between them iff their keys are equal.
    pub(crate) fn face_key(&self) -> &ThemeTypography {
        &self.applied
    }

    /// The applied keys, in table order, comma-joined (`-` for none): what
    /// `ctl theme` reports.
    pub(crate) fn applied_keys(&self) -> Vec<&'static str> {
        self.applied.keys()
    }

    /// The shadowed keys as `key:config|flag`, for `ctl theme`.
    pub(crate) fn shadowed_words(&self) -> Vec<String> {
        self.shadowed
            .iter()
            .map(|shadow| format!("{}:{}", shadow.key, shadow.origin.word()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::ThemeCatalog;

    fn cfg(text: &str) -> Config {
        aterm_toml::from_str(text).expect("config parses")
    }

    /// A catalog of one theme file `Studio` with `extra` typography lines.
    fn catalog(extra: &str) -> std::sync::Arc<ThemeCatalog> {
        let text = format!("foreground = #c0caf5\nbackground = #1a1b26\n{extra}\n");
        let parsed = aterm_types::scheme::parse_theme_str(&text).expect("theme parses");
        ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    }

    /// The config line that sets `key` to a valid value different from the
    /// theme example (so "config wins" is visible).
    fn config_line(key: &str) -> String {
        match key {
            "line_height" => "line_height = 1.4".into(),
            "font_size_scale" => "font_size_scale = 0.9".into(),
            "font_weight" => "font_weight = 300".into(),
            "ligatures" | "font_synthetic_style" => format!("{key} = false"),
            "font_features" | "fallback_fonts" => format!("{key} = [\"zero\"]"),
            _ => format!("{key} = \"Menlo\""),
        }
    }

    fn view_value(view: &Config, key: ThemeKey) -> Option<String> {
        match key {
            ThemeKey::FontFamily => view.font_family.clone(),
            ThemeKey::FontFamilyBold => view.font_family_bold.clone(),
            ThemeKey::FontFamilyItalic => view.font_family_italic.clone(),
            ThemeKey::FontFamilyBoldItalic => view.font_family_bold_italic.clone(),
            ThemeKey::FallbackFonts => view.fallback_fonts.as_ref().map(|l| l.0.join(", ")),
            ThemeKey::SymbolFont => view.symbol_font.clone(),
            ThemeKey::EmojiFont => view.emoji_font.clone(),
            ThemeKey::LineHeight => view.line_height.map(|v| v.to_string()),
            ThemeKey::Ligatures => view.ligatures.map(|v| v.to_string()),
            ThemeKey::FontFeatures => view.font_features.as_ref().map(|l| l.join(", ")),
            ThemeKey::FontSyntheticStyle => view.font_synthetic_style.map(|v| v.to_string()),
            ThemeKey::FontWeight => view.font_weight.map(|v| v.to_string()),
            ThemeKey::FontSizeScale => view.font_size_scale.map(|v| v.to_string()),
            _ => None,
        }
    }

    /// For every typography key: {theme sets, unset} × {config sets, unset}.
    /// A config value wins, a theme fills, the default stays, and `origin`
    /// and `shadowed` say which. Negative control: an overlay that writes the
    /// theme unconditionally breaks the config-wins row.
    #[test]
    fn overlay_precedence_matrix() {
        let rows = THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography);
        for spec in rows {
            let themed = catalog(&format!("{} = {}", spec.name, spec.example));
            let plain = catalog("");
            for (themes, theme_sets) in [(&themed, true), (&plain, false)] {
                for config_sets in [true, false] {
                    let mut text = String::from("theme = \"Studio\"\n");
                    if config_sets {
                        text.push_str(&config_line(spec.name));
                        text.push('\n');
                    }
                    let config = cfg(&text);
                    let typo = config.resolved_typography_with_flag(Appearance::Dark, themes, None);
                    let view = typo.overlay(&config);
                    let label = format!("{} theme={theme_sets} config={config_sets}", spec.name);
                    let authored = view_value(&config, spec.key);
                    let got = view_value(&view, spec.key);
                    match (theme_sets, config_sets) {
                        (_, true) => {
                            assert_eq!(got, authored, "{label}");
                            assert_eq!(typo.origin(spec.name, &config), TypeOrigin::Config);
                        }
                        (true, false) => {
                            assert!(got.is_some(), "{label}");
                            assert_eq!(typo.origin(spec.name, &config), TypeOrigin::Theme);
                        }
                        (false, false) => {
                            assert_eq!(got, None, "{label}");
                            assert_eq!(typo.origin(spec.name, &config), TypeOrigin::Default);
                        }
                    }
                    let shadowed: Vec<_> = typo.shadowed.iter().map(|s| s.key).collect();
                    let expect: Vec<&str> = if theme_sets && config_sets {
                        vec![spec.name]
                    } else {
                        Vec::new()
                    };
                    assert_eq!(shadowed, expect, "{label}");
                    // Negative control: an overlay that writes the theme's
                    // value regardless of the config loses the config's
                    // value in the config-wins row.
                    if theme_sets && config_sets {
                        let mut unconditional = config.clone();
                        let only_theme = ResolvedTypography {
                            applied: typo.declared.clone(),
                            ..ResolvedTypography::none(Appearance::Dark)
                        };
                        let theme_value =
                            view_value(&only_theme.overlay(&Config::default()), spec.key);
                        match spec.key {
                            ThemeKey::LineHeight => {
                                unconditional.line_height =
                                    typo.declared.line_height_milli.map(milli_f32);
                            }
                            ThemeKey::FontFamily => {
                                unconditional
                                    .font_family
                                    .clone_from(&typo.declared.font_family);
                            }
                            _ => continue,
                        }
                        assert_eq!(view_value(&unconditional, spec.key), theme_value);
                        assert_ne!(view_value(&unconditional, spec.key), authored, "{label}");
                    }
                }
            }
        }
    }

    /// `--font` beats a theme's family (reported as a flag shadow), and a
    /// config `display_font` keeps the family request while the theme's
    /// family is shadowed by it.
    #[test]
    fn flag_and_display_font_beat_a_theme_family() {
        let themes = catalog("font_family = Berkeley Mono");
        let config = cfg("theme = \"Studio\"\n");
        let typo = config.resolved_typography_with_flag(Appearance::Dark, &themes, Some("Menlo"));
        assert_eq!(
            typo.shadowed,
            vec![TypeShadow {
                key: "font_family",
                origin: TypeOrigin::Flag,
                by: "--font"
            }]
        );
        assert!(typo.applied.is_empty());
        let view = typo.overlay(&config);
        assert_eq!(
            crate::effective_font_family_with(Some("Menlo"), view.font_family_request().as_deref()),
            Some("Menlo".to_string())
        );
        let display = cfg("theme = \"Studio\"\ndisplay_font = \"pixel\"\n");
        let typo = display.resolved_typography_with_flag(Appearance::Dark, &themes, None);
        assert_eq!(typo.shadowed[0].by, "display_font");
        assert_eq!(
            typo.overlay(&display).font_family_request().as_deref(),
            Some("display:pixel")
        );
        // Negative control: with neither, the theme's family is requested.
        let typo = config.resolved_typography_with_flag(Appearance::Dark, &themes, None);
        assert_eq!(
            typo.overlay(&config).font_family_request().as_deref(),
            Some("Berkeley Mono")
        );
    }

    /// DTP §7: a theme with no typography leaves every font reader's input
    /// untouched, across the flag, family and display rungs and every kind of
    /// theme. Negative control: a theme `line_height` changes the knobs.
    #[test]
    fn empty_typography_is_byte_identical() {
        let empty_file = catalog("");
        for flag in [None, Some("Menlo")] {
            for family in [None, Some("Menlo")] {
                for display in [None, Some("pixel")] {
                    for theme in [None, Some("Dracula"), Some("Studio")] {
                        let mut text = String::new();
                        if let Some(f) = family {
                            text.push_str(&format!("font_family = {f:?}\n"));
                        }
                        if let Some(d) = display {
                            text.push_str(&format!("display_font = {d:?}\n"));
                        }
                        if let Some(t) = theme {
                            text.push_str(&format!("theme = {t:?}\n"));
                        }
                        let config = cfg(&text);
                        let typo = config.resolved_typography_with_flag(
                            Appearance::Dark,
                            &empty_file,
                            flag,
                        );
                        let view = typo.overlay(&config);
                        assert!(matches!(view, Cow::Borrowed(_)), "{text}");
                        assert!(*view == config);
                        let colors = config.resolved_colors(Appearance::Dark, &empty_file, None);
                        assert_eq!(
                            crate::app_config::RenderKnobs::from_config(&config, &colors),
                            crate::app_config::RenderKnobs::from_config(&view, &colors),
                        );
                        assert_same_readers(&config, &view, &text);
                    }
                }
            }
        }
        let mut px = crate::FONT_PX_MIN;
        while px <= crate::FONT_PX_MAX {
            assert_eq!(crate::scaled_font_px(px, 1000).to_bits(), px.to_bits());
            px += 0.5;
        }
        // Negative control.
        let themed = catalog("line_height = 1.2");
        let config = cfg("theme = \"Studio\"\n");
        let view = config
            .resolved_typography_with_flag(Appearance::Dark, &themed, None)
            .overlay(&config)
            .into_owned();
        let colors = config.resolved_colors(Appearance::Dark, &themed, None);
        assert_ne!(
            crate::app_config::RenderKnobs::from_config(&view, &colors),
            crate::app_config::RenderKnobs::from_config(&config, &colors)
        );
        assert!(
            crate::app_config::GlyphRasterKnobs::from_config(&view)
                != crate::app_config::GlyphRasterKnobs::from_config(&config)
        );
    }

    fn assert_same_readers(config: &Config, view: &Config, label: &str) {
        use crate::app_config::{FontConfig, GlyphRasterKnobs};
        assert!(
            FontConfig::from_config(config) == FontConfig::from_config(view),
            "{label}"
        );
        assert!(
            GlyphRasterKnobs::from_config(config) == GlyphRasterKnobs::from_config(view),
            "{label}"
        );
        assert_eq!(config.text_shaping(), view.text_shaping(), "{label}");
        assert_eq!(
            config.font_variation_requests(),
            view.font_variation_requests(),
            "{label}"
        );
        assert_eq!(
            config.font_family_request(),
            view.font_family_request(),
            "{label}"
        );
        assert_eq!(
            crate::app_config::resolve_font_px(config).to_bits(),
            crate::app_config::resolve_font_px(view).to_bits(),
            "{label}"
        );
    }

    /// A theme's `line_height = 1.15` is the same `f32` as a config's.
    #[test]
    fn theme_line_height_equals_config_line_height() {
        for text in ["1.15", "0.8", "1.333", "2.0", "1.05"] {
            let themes = catalog(&format!("line_height = {text}"));
            let config = cfg("theme = \"Studio\"\n");
            let view = config
                .resolved_typography_with_flag(Appearance::Dark, &themes, None)
                .overlay(&config)
                .into_owned();
            let direct = cfg(&format!("line_height = {text}\n"));
            assert_eq!(
                view.line_height.map(f32::to_bits),
                direct.line_height.map(f32::to_bits),
                "{text}"
            );
        }
    }

    /// Every feature list a theme accepts reaches the shaper whole: no spec
    /// is dropped by `Config::text_shaping`.
    #[test]
    fn theme_feature_lists_reach_the_shaper_whole() {
        for list in ["calt, -liga, ss01=2", "+zero ss02", "ss01 ss02 ss03 cv01=3"] {
            let themes = catalog(&format!("font_features = {list}"));
            let config = cfg("theme = \"Studio\"\n");
            let view = config
                .resolved_typography_with_flag(Appearance::Dark, &themes, None)
                .overlay(&config)
                .into_owned();
            let specs = view.font_features.clone().expect("filled");
            let shaped = view.text_shaping();
            let features = shaped
                .font_features
                .first()
                .map_or(0, |set| set.features.len());
            assert_eq!(features, specs.len(), "{list}");
        }
    }

    /// A theme's `bold_is_bright` is a colour rung under the config's key.
    /// Negative control: without the theme key the engine default stands.
    #[test]
    fn bold_is_bright_theme_rung() {
        let engine = aterm_core::config::TerminalConfig::default().bold_is_bright;
        let flipped = !engine;
        let themes = catalog(&format!("bold_is_bright = {flipped}"));
        let config = cfg("theme = \"Studio\"\n");
        assert_eq!(
            config
                .resolved_colors(Appearance::Dark, &themes, None)
                .bold_is_bright,
            flipped
        );
        let wins = cfg(&format!("theme = \"Studio\"\nbold_is_bright = {engine}\n"));
        assert_eq!(
            wins.resolved_colors(Appearance::Dark, &themes, None)
                .bold_is_bright,
            engine
        );
        let plain = catalog("");
        assert_eq!(
            config
                .resolved_colors(Appearance::Dark, &plain, None)
                .bold_is_bright,
            engine
        );
    }

    /// DTP §3.2's subset rule as a law: every theme key with an aterm.toml
    /// spelling is a real config key (setting it changes the config), and
    /// no typography key lacks one.
    #[test]
    fn config_subset_law() {
        for spec in &THEME_KEYS {
            if spec.group == ThemeKeyGroup::Typography {
                assert!(spec.config_key.is_some(), "{}", spec.name);
            }
            let Some(key) = spec.config_key else { continue };
            if key == "palette" {
                continue;
            }
            let value = match spec.value {
                // aterm.toml's `palette_generate` is a tri-state word, not the
                // theme file's flag.
                aterm_types::scheme::ThemeValueType::Flag if key == "palette_generate" => {
                    "\"never\"".to_string()
                }
                aterm_types::scheme::ThemeValueType::Flag => {
                    // Not the default, so the config visibly changes.
                    "false".to_string()
                }
                aterm_types::scheme::ThemeValueType::Milli { .. }
                | aterm_types::scheme::ThemeValueType::Integer { .. } => spec.example.to_string(),
                aterm_types::scheme::ThemeValueType::FontNameList
                | aterm_types::scheme::ThemeValueType::FeatureList => format!(
                    "[{}]",
                    spec.example
                        .split(',')
                        .map(|item| format!("{:?}", item.trim()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                _ => format!("{:?}", spec.example),
            };
            let parsed: Config = aterm_toml::from_str(&format!("{key} = {value}\n"))
                .unwrap_or_else(|e| panic!("{key} = {value}: {e}"));
            assert!(
                parsed != Config::default(),
                "{key} = {value} is not a config key"
            );
        }
        // Negative control: a key aterm.toml does not have leaves the default.
        let unknown: Config = aterm_toml::from_str("no_such_theme_key = 1\n").unwrap_or_default();
        assert!(unknown == Config::default());
    }

    /// `font_size_scale` in aterm.toml: clamped to 0.8..=1.25 with a notice
    /// when outside; absent and 1.0 are the identity.
    #[test]
    fn font_size_scale_config_key() {
        assert_eq!(Config::default().font_size_scale_milli_or_default(), 1000);
        assert_eq!(
            cfg("font_size_scale = 1.1").font_size_scale_milli_or_default(),
            1100
        );
        let wide = cfg("font_size_scale = 2.0");
        assert_eq!(wide.font_size_scale_milli_or_default(), 1250);
        assert!(crate::app_config::font_size_scale_load_notice(&wide).is_some());
        assert_eq!(
            crate::app_config::font_size_scale_load_notice(&cfg("font_size_scale = 1.1")),
            None
        );
        assert_eq!(crate::scaled_font_px(20.0, 1100), 22.0);
        assert_eq!(crate::scaled_font_px(24.0, 1100), (24.0_f32 * 1.1).round());
        assert_eq!(crate::scaled_font_px(24.0, 1000), 24.0);
    }
}
