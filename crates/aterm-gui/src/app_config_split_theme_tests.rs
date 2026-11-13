// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, ThemeCatalog, theme_misses, theme_selector_notice};
use aterm_types::Appearance;

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn theme_name_resolves_per_appearance() {
    for (source, dark, light) in [
        (
            r#"theme = "Tokyo Night""#,
            Some("Tokyo Night"),
            Some("Tokyo Night"),
        ),
        (
            r#"theme = "dark:Dracula,light:GitHub Light""#,
            Some("Dracula"),
            Some("GitHub Light"),
        ),
        (
            r#"theme = " DARK : Solarized Dark , Light : Solarized Light ""#,
            Some("Solarized Dark"),
            Some("Solarized Light"),
        ),
    ] {
        let c = cfg(source);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            dark,
            "{source}"
        );
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            light,
            "{source}"
        );
    }
    let split = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
    assert_eq!(
        split.theme_for(Appearance::Dark).bg,
        cfg(r#"theme = "Dracula""#).theme_for(Appearance::Dark).bg
    );
    assert_eq!(split.theme_for(Appearance::Light).bg, 0x00FF_FFFF);
}

#[test]
fn split_omitted_side_draws_the_named_side() {
    let c = cfg(r#"theme = "light:GitHub Light""#);
    for appearance in [Appearance::Dark, Appearance::Light] {
        assert_eq!(
            c.resolve_theme_name(appearance).as_deref(),
            Some("GitHub Light")
        );
    }
    assert_eq!(c.theme_for(Appearance::Dark).bg, 0x00FF_FFFF);
    assert!(theme_misses(&c, &ThemeCatalog::empty()).is_empty());
}

#[test]
fn split_omitted_side_draws_the_builtin_pair() {
    let c = cfg(r#"theme = "dark:Gruvbox Dark""#);
    assert_eq!(
        c.resolve_theme_name(Appearance::Light).as_deref(),
        Some("Gruvbox Light")
    );
    assert_eq!(
        c.theme_for(Appearance::Light),
        cfg(r#"theme = "Gruvbox Light""#).theme_for(Appearance::Light)
    );
    assert_ne!(
        c.theme_for(Appearance::Light),
        c.theme_for(Appearance::Dark)
    );
    let user = cfg(r#"theme = "dark:Studio""#);
    assert_eq!(
        user.resolve_theme_name(Appearance::Light).as_deref(),
        Some("Studio")
    );
    let notice = theme_selector_notice(Some("dark:Gruvbox Dark,lite:x")).expect("an issue");
    assert!(
        notice.ends_with(r#"light mode uses "Gruvbox Light""#),
        "{notice}"
    );
}

#[test]
fn theme_misses_include_selector_issues() {
    let themes = ThemeCatalog::empty();
    let misses = |theme: &str| {
        theme_misses(
            &Config {
                theme: Some(theme.to_string()),
                ..Default::default()
            },
            &themes,
        )
    };
    let selector = |theme: &str| theme_selector_notice(Some(theme));
    let c = cfg(r#"theme = "dark:Nord,dark:Dracula""#);
    assert_eq!(
        c.resolve_theme_name(Appearance::Dark).as_deref(),
        Some("Nord")
    );
    assert_eq!(
        misses("dark:Nord,dark:Dracula"),
        vec![selector("dark:Nord,dark:Dracula").unwrap()]
    );
    assert_eq!(misses("dark:Nord,lite:Dracula").len(), 1);
    assert_eq!(misses("dark:Nord,light:Nope,light:Dracula").len(), 2);
    for clean in ["Nord", "dark:Nord,light:GitHub Light", "dark:Nord"] {
        assert_eq!(selector(clean), None, "{clean:?}");
        assert!(misses(clean).is_empty(), "{clean:?}");
    }
    assert!(theme_selector_notice(None).is_none());
}

#[test]
fn applied_terminal_config_tracks_split() {
    let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
    let rgb = |packed: u32| {
        aterm_types::Rgb::new(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        )
    };
    assert_ne!(
        c.applied_terminal_config_for(Appearance::Dark)
            .default_background,
        c.applied_terminal_config_for(Appearance::Light)
            .default_background
    );
    for side in [Appearance::Dark, Appearance::Light] {
        let engine = c.applied_terminal_config_for(side);
        let theme = c.theme_for(side);
        assert_eq!(engine.default_background, rgb(theme.bg), "{side:?}");
        assert_eq!(
            engine.cursor_color,
            Some(rgb(theme.cursor)),
            "{side:?}: the OSC 112 reset baseline"
        );
        assert_eq!(
            engine.selection_background,
            Some(rgb(theme.selection)),
            "{side:?}: the OSC 117 reset baseline"
        );
    }
}

#[test]
fn applied_terminal_config_preserves_selected_text_override() {
    let c = cfg("selection_foreground = \"#123456\"");
    assert_eq!(
        c.applied_terminal_config().selection_foreground,
        Some(aterm_types::Rgb::new(0x12, 0x34, 0x56))
    );
}
