// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
                if theme_sets && config_sets {
                    let mut unconditional = config.clone();
                    let only_theme = ResolvedTypography {
                        applied: typo.declared.clone(),
                        ..ResolvedTypography::none(Appearance::Dark)
                    };
                    let theme_value = view_value(&only_theme.overlay(&Config::default()), spec.key);
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
/// config `display_font` draws its layer over the theme's family, which
/// stays the font.
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
    assert!(typo.shadowed.is_empty());
    let view = typo.overlay(&display);
    assert_eq!(view.font_family_request().as_deref(), Some("Berkeley Mono"));
    assert_eq!(view.display_faces_request(), ["pixel"]);
    let typo = config.resolved_typography_with_flag(Appearance::Dark, &themes, None);
    assert_eq!(
        typo.overlay(&config).font_family_request().as_deref(),
        Some("Berkeley Mono")
    );
}

/// A theme with no typography leaves every font reader's input
/// untouched, across the flag, family and display rungs and every kind of
/// theme.
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
                    let typo =
                        config.resolved_typography_with_flag(Appearance::Dark, &empty_file, flag);
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

/// Every theme key with an aterm.toml spelling is a real config key
/// (setting it changes the config), and no typography key lacks one.
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
            aterm_types::scheme::ThemeValueType::Flag if key == "palette_generate" => {
                "\"never\"".to_string()
            }
            aterm_types::scheme::ThemeValueType::Flag => "false".to_string(),
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
