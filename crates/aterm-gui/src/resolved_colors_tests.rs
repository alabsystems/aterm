// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ColorOrigin, ResolvedColors, ThemeSource};
use crate::app_config::{Config, ThemeCatalog};
use aterm_core::config::TerminalConfig;
use aterm_core::terminal::{ColorPalette, Rgb};
use aterm_render::Theme;
use aterm_types::{Appearance, ColorScheme};

fn cfg(text: &str) -> Config {
    aterm_toml::from_str(text).expect("test config parses")
}

fn resolve(config: &Config, appearance: Appearance) -> ResolvedColors {
    config.resolved_colors(appearance, &ThemeCatalog::default(), None)
}

fn engine_view(mut tc: TerminalConfig) -> (TerminalConfig, Option<Vec<Rgb>>) {
    let palette = tc
        .custom_palette
        .take()
        .map(|p| (0..=255u8).map(|i| p.get(i)).collect());
    (tc, palette)
}

#[test]
fn resolved_colors_projections_agree_on_every_case() {
    let mut selectors: Vec<Option<String>> = vec![
        None,
        Some("no-such-theme".to_string()),
        Some("dark:Dracula,light:Solarized Light".to_string()),
    ];
    selectors.extend(
        aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| Some(name.to_string())),
    );
    let overrides = [
        "",
        "foreground = \"#102030\"",
        "background = \"#f0e0d0\"",
        "cursor_color = \"#ff00ff\"",
        "selection_color = \"#334455\"",
        "selection_foreground = \"#ffffff\"",
        "palette = [\"#010203\", \"#aabbcc\"]",
        "palette = [\"#000000\", \"#800000\", \"nope\", \"#123456\"]",
        "palette = [\"bad!\"]",
        "palette = [\"#abc\"]",
        "bold_is_bright = false",
        "foreground = \"#zz0000\"\nbackground = \"oops\"",
        "foreground = \"#102030\"\nbackground = \"#203040\"\ncursor_color = \"#304050\"\n\
         selection_color = \"#405060\"\nselection_foreground = \"#506070\"\n\
         palette = [\"#111111\", \"#222222\", \"#333333\", \"#444444\", \"#555555\", \
         \"#666666\", \"#777777\", \"#888888\", \"#999999\", \"#aaaaaa\", \"#bbbbbb\", \
         \"#cccccc\", \"#dddddd\", \"#eeeeee\", \"#efefef\", \"#fefefe\", \"#121212\"]",
    ];
    let unpack = |c: u32| {
        Rgb::new(
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        )
    };
    let parsed = |v: &Option<String>| v.as_deref().and_then(aterm_types::scheme::parse_hex);
    let themes = ThemeCatalog::default();
    let mut cases = 0;
    for selector in &selectors {
        for extra in overrides {
            let mut config = cfg(extra);
            config.theme.clone_from(selector);
            for appearance in [Appearance::Dark, Appearance::Light] {
                let label = format!("{selector:?} / {extra:?} / {appearance:?}");
                let r = config.resolved_colors(appearance, &themes, None);
                let t = config.theme_for_with_assets(appearance, &themes);
                let tc = config.applied_terminal_config_for_with_assets(appearance, &themes);
                assert_eq!(unpack(t.fg), tc.default_foreground, "fg: {label}");
                assert_eq!(unpack(t.bg), tc.default_background, "bg: {label}");
                assert_eq!(Some(unpack(t.cursor)), tc.cursor_color, "cursor: {label}");
                assert_eq!(
                    Some(unpack(t.selection)),
                    tc.selection_background,
                    "selection: {label}"
                );
                assert_eq!(tc.default_foreground, r.scheme.foreground, "{label}");
                assert_eq!(tc.default_background, r.scheme.background, "{label}");
                assert_eq!(tc.selection_foreground, r.selection_fg(), "{label}");
                assert_eq!(tc.bold_is_bright, r.bold_is_bright, "{label}");
                let engine = engine_view(tc.clone()).1;
                let resolved = r
                    .custom_palette()
                    .map(|p| (0..=255u8).map(|i| p.get(i)).collect::<Vec<_>>());
                assert_eq!(engine, resolved, "palette: {label}");
                for (key, value, drawn) in [
                    (
                        "foreground",
                        &config.foreground,
                        Some(tc.default_foreground),
                    ),
                    (
                        "background",
                        &config.background,
                        Some(tc.default_background),
                    ),
                    ("cursor_color", &config.cursor_color, tc.cursor_color),
                    (
                        "selection_color",
                        &config.selection_color,
                        tc.selection_background,
                    ),
                    (
                        "selection_foreground",
                        &config.selection_foreground,
                        tc.selection_foreground,
                    ),
                ] {
                    if let Some(want) = parsed(value) {
                        assert_eq!(drawn, Some(want), "{key}: {label}");
                    }
                }
                for (i, hex) in config.palette.iter().flatten().enumerate() {
                    if let Some(want) = aterm_types::scheme::parse_hex(hex) {
                        let got = engine.as_ref().map(|p| p[i]);
                        assert_eq!(got, Some(want), "palette[{i}]: {label}");
                    }
                }
                cases += 1;
            }
        }
    }
    assert!(cases > 100, "the matrix ran: {cases}");
}

#[test]
fn resolved_colors_unset_theme_is_the_dark_default() {
    let themes = ThemeCatalog::default();
    let explicit = cfg("theme = \"Default\"");
    let default = resolve(&explicit, Appearance::Light);
    for unset in [
        Config::default(),
        cfg("theme = \"\""),
        cfg("theme = \"dark:,light:\""),
    ] {
        let label = format!("{:?}", unset.theme);
        let light = resolve(&unset, Appearance::Light);
        assert_eq!(light.id, "Default", "{label}");
        assert_eq!(light.source, ThemeSource::Builtin, "{label}");
        assert_eq!(light.scheme, default.scheme, "{label}");
        assert_eq!(light.scheme, ColorScheme::default(), "{label}");
        assert_eq!(light.reported, Appearance::Dark, "{label}");
        assert_eq!(
            light.custom_palette(),
            None,
            "{label}: the engine keeps its table"
        );
        assert_eq!(
            unset.theme_for_with_assets(Appearance::Light, &themes),
            explicit.theme_for_with_assets(Appearance::Light, &themes),
            "{label}"
        );
        assert_eq!(
            unset.applied_terminal_config_for_with_assets(Appearance::Light, &themes),
            unset.applied_terminal_config_for_with_assets(Appearance::Dark, &themes),
            "{label}: one engine config on both appearances"
        );
    }

    let pair = resolve(
        &cfg("theme = \"dark:Default,light:Default Light\""),
        Appearance::Light,
    );
    assert_eq!(pair.id, "Default Light");
    assert_eq!(pair.reported, Appearance::Light);
    assert_ne!(pair.scheme.background, default.scheme.background);
}

#[test]
fn resolved_colors_reports_a_bad_hex_once() {
    let config = cfg(
        "theme = \"Dracula\"\nforeground = \"#zzzzzz\"\nbackground = \"#102030\"\n\
         palette = [\"#010203\", \"nope\", \"#040506\"]",
    );
    let resolved = resolve(&config, Appearance::Dark);
    let keys: Vec<&str> = resolved.issues.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["foreground", "palette[1]"]);
    let dracula = aterm_types::scheme::builtin("Dracula").unwrap();
    assert_eq!(resolved.scheme.foreground, dracula.foreground);
    assert_eq!(resolved.provenance.foreground, ColorOrigin::Theme);
    assert_eq!(resolved.scheme.background, Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(resolved.scheme.ansi[1], dracula.ansi[1]);
    assert_eq!(resolved.scheme.ansi[2], Rgb::new(4, 5, 6));
    let warnings = crate::diagnostics::validate_config_text(
        "foreground = \"#zzzzzz\"\npalette = [\"#010203\", \"nope\"]\n",
    )
    .expect("valid TOML")
    .join("\n");
    assert_eq!(
        warnings.matches("foreground: expected").count(),
        1,
        "{warnings}"
    );
    assert_eq!(warnings.matches("palette[1]").count(), 1, "{warnings}");
    assert!(
        resolve(&cfg("foreground = \"#102030\""), Appearance::Dark)
            .issues
            .is_empty()
    );
}

#[test]
fn resolved_colors_accepts_short_hex() {
    let resolved = resolve(
        &cfg("foreground = \"#fff\"\nbackground = \"#123\"\npalette = [\"#f00\"]"),
        Appearance::Dark,
    );
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
    assert_eq!(resolved.scheme.foreground, Rgb::new(0xff, 0xff, 0xff));
    assert_eq!(resolved.scheme.background, Rgb::new(0x11, 0x22, 0x33));
    assert_eq!(resolved.scheme.ansi[0], Rgb::new(0xff, 0, 0));
    assert_eq!(resolved.theme().fg, 0x00ff_ffff);
    let bad = resolve(&cfg("foreground = \"#ffff\""), Appearance::Dark);
    assert_eq!(bad.issues.len(), 1);
    assert_eq!(bad.scheme.foreground, ColorScheme::default().foreground);
}

#[test]
fn resolved_colors_chrome_keys_beat_the_theme() {
    let mut scheme = aterm_types::scheme::builtin("Dracula").unwrap();
    scheme.name = "Mine".to_string();
    scheme.chrome.accent = Some(Rgb::new(0x11, 0x22, 0x33));
    scheme.chrome.border = Some(Rgb::new(0x44, 0x55, 0x66));
    let themes = ThemeCatalog::from_schemes([("Mine".to_string(), scheme)]);
    let at = |text: &str| cfg(text).resolved_colors(Appearance::Dark, &themes, None);
    let won = at("theme = \"Mine\"\naccent_color = \"#ff00aa\"");
    assert_eq!(won.scheme.chrome.accent, Some(Rgb::new(0xff, 0x00, 0xaa)));
    assert_eq!(won.provenance.chrome.accent, ColorOrigin::Config);
    assert_eq!(won.config_keys(), vec!["accent_color".to_string()]);
    assert_eq!(won.authored_chrome().accent, Some([0xff, 0x00, 0xaa]));
    assert_eq!(won.scheme.chrome.border, Some(Rgb::new(0x44, 0x55, 0x66)));
    assert_eq!(won.provenance.chrome.border, ColorOrigin::Theme);
    assert_eq!(won.provenance.chrome.search_match, ColorOrigin::Default);
    let bad = at("theme = \"Mine\"\naccent_color = \"#nope\"");
    assert_eq!(bad.issues.len(), 1, "{:?}", bad.issues);
    assert_eq!(bad.issues[0].key, "accent_color");
    assert_eq!(bad.scheme.chrome.accent, Some(Rgb::new(0x11, 0x22, 0x33)));
    assert_eq!(bad.provenance.chrome.accent, ColorOrigin::Theme);
    let theirs = at("theme = \"Mine\"");
    assert_eq!(theirs.provenance.chrome.accent, ColorOrigin::Theme);
    assert!(theirs.config_keys().is_empty());
    let all = cfg(
        "accent_color = \"#010101\"\nactive_tab_color = \"#020202\"\n\
                   border_color = \"#030303\"\nsearch_match_color = \"#040404\"",
    )
    .resolved_colors(Appearance::Dark, &ThemeCatalog::default(), None);
    assert_eq!(
        all.config_keys(),
        super::CHROME_KEYS
            .map(|(name, _)| name.to_string())
            .to_vec()
    );
    assert_eq!(
        all.authored_chrome(),
        aterm_messages::tokens::AuthoredChrome {
            accent: Some([1; 3]),
            active_tab: Some([2; 3]),
            border: Some([3; 3]),
            search_match: Some([4; 3]),
        }
    );
}

#[test]
fn resolved_colors_provenance_names_each_slot() {
    let none = resolve(&Config::default(), Appearance::Dark);
    assert_eq!(none.source, ThemeSource::Builtin);
    assert_eq!(none.id, "Default");
    assert_eq!(none.provenance.foreground, ColorOrigin::Default);
    assert_eq!(none.provenance.selection_fg, ColorOrigin::Default);
    assert!(none.config_keys().is_empty());
    assert_eq!(
        none.custom_palette(),
        None,
        "no theme, no palette: engine table"
    );

    let themed = resolve(
        &cfg("theme = \"dracula\"\ncursor_color = \"#ff00ff\"\npalette = [\"#000001\"]"),
        Appearance::Dark,
    );
    assert_eq!(themed.id, "Dracula", "canonical built-in name");
    assert_eq!(themed.provenance.foreground, ColorOrigin::Theme);
    assert_eq!(themed.provenance.cursor, ColorOrigin::Config);
    assert_eq!(themed.provenance.ansi[0], ColorOrigin::Config);
    assert_eq!(themed.provenance.ansi[1], ColorOrigin::Theme);
    assert_eq!(themed.config_keys(), ["cursor_color", "palette[0]"]);

    let extended = resolve(
        &cfg(&format!(
            "palette = [{}\"#0a0b0c\"]",
            "\"#000000\", ".repeat(20)
        )),
        Appearance::Dark,
    );
    assert_eq!(
        extended.extended_palette.last(),
        Some(&(20, Rgb::new(10, 11, 12)))
    );
    let palette = extended.custom_palette().expect("a palette entry landed");
    assert_eq!(palette.get(20), Rgb::new(10, 11, 12));
    assert_eq!(palette.get(21), ColorPalette::default_color(21));

    let missing = resolve(&cfg("theme = \"no-such-theme\""), Appearance::Dark);
    assert!(matches!(
        missing.source,
        ThemeSource::DefaultFallback { .. }
    ));
    assert_eq!(missing.provenance.background, ColorOrigin::Default);
    assert_eq!(missing.scheme.background, ColorScheme::default().background);
}

#[test]
fn resolved_colors_reads_a_file_theme_and_its_selection_foreground() {
    let scheme = ColorScheme {
        name: "Mine".into(),
        selection_fg: Some(Rgb::new(1, 2, 3)),
        ..ColorScheme::default()
    };
    let themes = ThemeCatalog::from_schemes([("Mine".to_string(), scheme)]);
    let config = cfg("theme = \"mine\"");
    let resolved = config.resolved_colors(Appearance::Dark, &themes, None);
    assert_eq!(resolved.source, ThemeSource::File("Mine".into()));
    assert_eq!(resolved.id, "Mine");
    assert_eq!(resolved.provenance.selection_fg, ColorOrigin::Theme);
    assert_eq!(
        config
            .applied_terminal_config_from(&resolved)
            .selection_foreground,
        Some(Rgb::new(1, 2, 3))
    );
    let over = cfg("theme = \"mine\"\nselection_foreground = \"#040506\"").resolved_colors(
        Appearance::Dark,
        &themes,
        None,
    );
    assert_eq!(over.selection_fg(), Some(Rgb::new(4, 5, 6)));
    assert_eq!(over.provenance.selection_fg, ColorOrigin::Config);
}

#[test]
fn resolved_colors_candidate_replaces_the_configured_theme() {
    let config = cfg("theme = \"Dracula\"\nforeground = \"#102030\"");
    let preview = config.resolved_colors(Appearance::Dark, &ThemeCatalog::default(), Some("Nord"));
    assert_eq!(preview.id, "Nord");
    let nord = aterm_types::scheme::builtin("Nord").unwrap();
    assert_eq!(preview.scheme.background, nord.background);
    assert_eq!(preview.scheme.foreground, Rgb::new(0x10, 0x20, 0x30));
    let split = config.resolved_colors(
        Appearance::Light,
        &ThemeCatalog::default(),
        Some("dark:Nord,light:Solarized Light"),
    );
    assert_eq!(split.id, "Solarized Light");
    assert_eq!(split.side, Appearance::Light);
}

#[test]
fn resolved_colors_reported_follows_the_background_on_screen() {
    let light = "Solarized Light";
    assert_eq!(
        aterm_types::scheme::builtin(light).unwrap().appearance,
        Appearance::Light
    );
    let plain = resolve(&cfg(&format!("theme = \"{light}\"")), Appearance::Dark);
    assert_eq!(plain.reported, Appearance::Light, "the theme's appearance");
    let fg_only = resolve(
        &cfg(&format!("theme = \"{light}\"\nforeground = \"#000000\"")),
        Appearance::Dark,
    );
    assert_eq!(fg_only.reported, Appearance::Light);
    let dark_bg = resolve(
        &cfg(&format!("theme = \"{light}\"\nbackground = \"#101010\"")),
        Appearance::Light,
    );
    assert_eq!(
        dark_bg.reported,
        Appearance::Dark,
        "a dark background reads dark"
    );
    let light_bg = resolve(&cfg("background = \"#f8f8f8\""), Appearance::Dark);
    assert_eq!(light_bg.reported, Appearance::Light);
    let bad_bg = resolve(&cfg("background = \"#f8f8\""), Appearance::Dark);
    assert_eq!(bad_bg.reported, Appearance::Dark);
}

#[test]
fn resolved_colors_derives_an_unset_selection_from_the_final_colours() {
    let light = aterm_types::scheme::parse_scheme_str(
        "appearance = light\nforeground = #24292f\nbackground = #fafafa\n",
    )
    .expect("a minimal light theme parses");
    let themes = ThemeCatalog::from_schemes([("paper".to_string(), light)]);
    let plain = cfg("theme = \"paper\"").resolved_colors(Appearance::Light, &themes, None);
    let (fg, bg) = (plain.scheme.foreground, plain.scheme.background);
    let band = plain.scheme.selection_band();
    assert!(
        band.contrast(bg) < band.contrast(fg),
        "a light band: {band:?}"
    );
    assert!(band.contrast(bg) >= aterm_types::scheme::MIN_SELECTION_CONTRAST);
    let config = cfg("theme = \"paper\"");
    let engine = config.applied_terminal_config_from(&plain);
    assert_eq!(engine.selection_background, Some(band));
    assert_eq!(
        plain.theme().selection,
        Theme::from(&plain.scheme).selection
    );

    let over = cfg("theme = \"paper\"\nbackground = \"#e0e0e0\"").resolved_colors(
        Appearance::Light,
        &themes,
        None,
    );
    assert_eq!(
        over.scheme.selection_band(),
        aterm_types::scheme::derived_selection(fg, Rgb::new(0xe0, 0xe0, 0xe0)),
        "the band follows the background on screen"
    );
}

#[test]
fn split_override_warnings_name_the_side_an_override_breaks() {
    let themes = ThemeCatalog::default();
    let warnings = cfg("theme = \"dark:Dracula,light:GitHub Light\"\nbackground = \"#000\"")
        .split_override_warnings(&themes);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (key, message) = &warnings[0];
    assert_eq!(*key, "background");
    assert!(message.contains("light mode"), "{message}");
    assert!(message.contains("\"GitHub Light\""), "{message}");
    assert!(!message.contains("dark mode"), "{message}");

    let palette =
        cfg("theme = \"dark:Dracula,light:GitHub Light\"\npalette = [\"#000000\", \"#ffb0b0\"]")
            .split_override_warnings(&themes);
    assert_eq!(palette.len(), 1, "{palette:?}");
    assert_eq!(palette[0].0, "palette");
    assert!(palette[0].1.contains("palette[1]"), "{palette:?}");
    assert!(!palette[0].1.contains("palette[0]"), "{palette:?}");

    for quiet in [
        "theme = \"GitHub Light\"\nbackground = \"#000\"",
        "theme = \"Dracula\"\nbackground = \"#000\"",
        "theme = \"dark:Dracula,light:GitHub Light\"",
        "theme = \"dark:Dracula,light:GitHub Light\"\nbackground = \"#808080\"\nforeground = \"#000000\"",
    ] {
        let found = cfg(quiet).split_override_warnings(&themes);
        assert!(found.is_empty(), "{quiet}: {found:?}");
    }
}

#[test]
fn palette_overrides_set_one_slot_and_leave_the_rest_to_the_theme() {
    let sparse = cfg("palette_overrides = { 12 = \"#123456\" }");
    let themes = ThemeCatalog::default();
    for theme in ["Nord", "Dracula"] {
        let resolved = sparse.resolved_colors(Appearance::Dark, &themes, Some(theme));
        let alone = cfg("").resolved_colors(Appearance::Dark, &themes, Some(theme));
        assert_eq!(resolved.scheme.ansi[12], Rgb::new(0x12, 0x34, 0x56));
        for slot in (0..16).filter(|slot| *slot != 12) {
            assert_eq!(
                resolved.scheme.ansi[slot], alone.scheme.ansi[slot],
                "{theme} {slot}"
            );
            assert_eq!(resolved.provenance.ansi[slot], ColorOrigin::Theme);
        }
        assert_eq!(resolved.provenance.ansi[12], ColorOrigin::Config);
        assert_eq!(resolved.config_keys(), ["palette_overrides.12"]);
    }
    let pinned = cfg(&format!(
        "palette = [{}\"#123456\"]",
        "\"#000000\", ".repeat(12)
    ))
    .resolved_colors(Appearance::Dark, &themes, Some("Dracula"));
    let dracula = cfg("").resolved_colors(Appearance::Dark, &themes, Some("Dracula"));
    assert_ne!(
        pinned.scheme.ansi[1], dracula.scheme.ansi[1],
        "the list pins the slots before the one it reaches"
    );
}

#[test]
fn palette_overrides_land_over_the_list_and_report_bad_entries() {
    let resolved = resolve(
        &cfg("palette = [\"#000001\", \"#000002\"]\n\
              [palette_overrides]\n\
              1 = \"#0000ff\"\n\
              10 = \"#00000a\"\n\
              2 = \"#000003\"\n\
              200 = \"#0a0b0c\"\n\
              07 = \"#ffffff\"\n\
              x = \"#ffffff\"\n\
              3 = \"nope\"\n"),
        Appearance::Dark,
    );
    assert_eq!(resolved.scheme.ansi[0], Rgb::new(0, 0, 1));
    assert_eq!(resolved.scheme.ansi[1], Rgb::new(0, 0, 0xff));
    assert_eq!(resolved.scheme.ansi[2], Rgb::new(0, 0, 3));
    assert_eq!(resolved.scheme.ansi[10], Rgb::new(0, 0, 0x0a));
    assert_eq!(resolved.extended_palette, vec![(200, Rgb::new(10, 11, 12))]);
    assert_eq!(
        resolved.custom_palette().expect("a palette").get(200),
        Rgb::new(10, 11, 12)
    );
    assert_eq!(
        resolved.config_keys(),
        [
            "palette[0]",
            "palette[1]",
            "palette_overrides.1",
            "palette_overrides.2",
            "palette_overrides.10",
            "palette_overrides.200",
        ]
    );
    let issues: Vec<&str> = resolved.issues.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        issues,
        [
            "palette_overrides.07",
            "palette_overrides.3",
            "palette_overrides.x"
        ]
    );
}

#[test]
fn resolved_colors_keeps_the_theme_before_the_colour_overrides() {
    let palette: Vec<String> = (0..256)
        .map(|i| format!("\"#{:02x}{:02x}{:02x}\"", i, 255 - i, i / 2))
        .collect();
    let extras = format!(
        "palette = [{}]\nselection_foreground = \"#123456\"\n[palette_overrides]\n3 = \"#ff0000\"\n",
        palette.join(", ")
    );
    let overrides = "foreground = \"#ffaa00\"\nbackground = \"#002244\"\ncursor_color = \"#00ff00\"\nselection_color = \"#440044\"\n";
    for theme in ["", "theme = \"dark:Dracula,light:Solarized Light\"\n"] {
        for candidate in [None, Some("Nord")] {
            for side in [Appearance::Dark, Appearance::Light] {
                let bare = cfg(&format!("{theme}{extras}"));
                let set = cfg(&format!("{theme}{overrides}{extras}"));
                let themes = ThemeCatalog::default();
                let bare = bare.resolved_colors(side, &themes, candidate);
                let set = set.resolved_colors(side, &themes, candidate);
                assert_eq!(
                    set.theme_before_overrides(),
                    bare.theme(),
                    "{theme:?} {candidate:?} {side:?}: the theme before the keys"
                );
                assert_eq!(
                    bare.theme_before_overrides(),
                    bare.theme(),
                    "with no colour key the two are the same"
                );
                assert_ne!(set.theme(), set.theme_before_overrides());
                assert_eq!(set.theme().fg, 0x00FF_AA00);
            }
        }
    }
}

fn generated_100(config: &Config, theme: Option<&str>) -> Rgb {
    let mut scheme = config
        .resolved_colors(Appearance::Dark, &ThemeCatalog::default(), theme)
        .scheme;
    scheme.palette_generate = true;
    scheme.to_color_palette().get(100)
}

#[test]
fn palette_generate_follows_the_theme_unless_the_config_says() {
    let themes = ThemeCatalog::default();
    let plain = cfg("");
    let night = plain.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
    assert!(night.palette_generate);
    assert_eq!(night.palette_generate_origin, ColorOrigin::Theme);
    let palette = night
        .custom_palette()
        .expect("a theme hands its palette over");
    assert_eq!(palette.get(100), generated_100(&plain, Some("Aterm Night")));
    assert_ne!(palette.get(100), ColorPalette::default_color(100));

    let never = cfg("palette_generate = \"never\"");
    let kept = never.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
    assert!(!kept.palette_generate);
    assert_eq!(kept.palette_generate_origin, ColorOrigin::Config);
    assert_eq!(
        kept.custom_palette().expect("palette").get(100),
        ColorPalette::default_color(100)
    );

    let nord = plain.resolved_colors(Appearance::Dark, &themes, Some("Nord"));
    assert!(!nord.palette_generate);
    assert_eq!(
        nord.custom_palette().expect("palette").get(100),
        ColorPalette::default_color(100)
    );

    assert_eq!(resolve(&plain, Appearance::Dark).custom_palette(), None);
    let always = cfg("palette_generate = \"always\"");
    let unset = resolve(&always, Appearance::Dark);
    assert!(unset.palette_generate);
    assert_eq!(unset.palette_generate_origin, ColorOrigin::Config);
    assert_eq!(
        unset.custom_palette().expect("derived").get(100),
        generated_100(&always, None)
    );
    let odd = cfg("palette_generate = \"sometimes\"");
    assert!(
        odd.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"))
            .palette_generate
    );
}

#[test]
fn config_slots_win_over_derived_ones_and_the_cube_follows_ansi_overrides() {
    let themes = ThemeCatalog::default();
    let config = cfg("palette_overrides = { 100 = \"#123456\", 9 = \"#00ff00\" }");
    let resolved = config.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
    let palette = resolved.custom_palette().expect("palette");
    assert_eq!(palette.get(100), Rgb::new(0x12, 0x34, 0x56));
    assert_eq!(palette.get(196), Rgb::new(0x00, 0xff, 0x00));
    assert_eq!(
        palette.get(101),
        {
            let mut scheme = resolved.scheme.clone();
            scheme.palette_generate = true;
            scheme.to_color_palette().get(101)
        },
        "the neighbour stays derived"
    );
    use super::SlotOrigin;
    assert_eq!(resolved.extended_origin(100), SlotOrigin::Config);
    assert_eq!(resolved.extended_origin(101), SlotOrigin::Generated);
    assert_eq!(
        resolved.extended_origin(9),
        SlotOrigin::Ansi(ColorOrigin::Config)
    );
    assert_eq!(
        resolved.extended_origin(1),
        SlotOrigin::Ansi(ColorOrigin::Theme)
    );
    let off = cfg("palette_generate = \"never\"\npalette_overrides = { 100 = \"#123456\" }")
        .resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
    assert_eq!(off.extended_origin(101), SlotOrigin::Xterm);
    assert_eq!(off.extended_origin(100), SlotOrigin::Config);
}
