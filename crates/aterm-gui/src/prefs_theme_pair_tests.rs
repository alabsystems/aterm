// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ThemeFollowKept, ThemeMode, theme_always_value, theme_choice_value, theme_follow_system_value,
    theme_is_unset, theme_mode, theme_sideless_choice_value, theme_slots, theme_with_side,
};
use aterm_types::Appearance::{Dark, Light};

fn builtins() -> std::sync::Arc<crate::app_config::ThemeCatalog> {
    crate::app_config::ThemeCatalog::empty()
}

#[test]
fn the_mode_is_read_off_the_value() {
    assert_eq!(theme_mode(None), ThemeMode::Always);
    assert_eq!(theme_mode(Some("")), ThemeMode::Always);
    assert_eq!(theme_mode(Some("  ")), ThemeMode::Always);
    assert_eq!(theme_mode(Some("dark:,light:")), ThemeMode::Always);
    assert_eq!(theme_mode(Some("Nord")), ThemeMode::Always);
    assert_eq!(
        theme_mode(Some("dark:Nord,light:Nord Light")),
        ThemeMode::FollowSystem
    );
    assert_eq!(
        theme_mode(Some("light:Nord Light")),
        ThemeMode::FollowSystem
    );
}

#[test]
fn turning_follow_system_on_seeds_the_other_side_from_the_pair() {
    let themes = builtins();
    assert_eq!(
        theme_follow_system_value(Some("Gruvbox Dark"), None, &themes),
        "dark:Gruvbox Dark,light:Gruvbox Light"
    );
    assert_eq!(
        theme_follow_system_value(Some("Gruvbox Light"), None, &themes),
        "dark:Gruvbox Dark,light:Gruvbox Light"
    );
    for raw in [
        Some("Default"),
        Some("Default Light"),
        None,
        Some(""),
        Some("dark:,light:"),
    ] {
        assert_eq!(
            theme_follow_system_value(raw, None, &themes),
            "dark:Default,light:Default Light",
            "{raw:?}"
        );
    }
    let unpaired = aterm_types::scheme::builtin_theme_list()
        .into_iter()
        .filter(|theme| theme.pair.is_none())
        .collect::<Vec<_>>();
    for appearance in [Dark, Light] {
        let theme = unpaired
            .iter()
            .find(|theme| theme.appearance == appearance)
            .expect("a built-in without a pair on each side");
        let expected = match appearance {
            Dark => format!("dark:{},light:Default Light", theme.name),
            Light => format!("dark:Default,light:{}", theme.name),
        };
        assert_eq!(
            theme_follow_system_value(Some(theme.name), None, &themes),
            expected
        );
    }
    assert_eq!(
        theme_follow_system_value(Some("Nord"), None, &themes),
        "dark:Nord,light:Default Light"
    );
    assert_eq!(
        theme_follow_system_value(Some("GitHub Light"), None, &themes),
        "dark:Default,light:GitHub Light"
    );
    assert_eq!(
        theme_follow_system_value(Some("Office Missing"), None, &themes),
        "dark:Office Missing,light:Default Light"
    );
    assert_eq!(
        theme_follow_system_value(Some("dark:Dracula,light:Nord"), None, &themes),
        "dark:Dracula,light:Nord"
    );
}

#[test]
fn a_choice_keeps_a_pair_and_writes_a_name_otherwise() {
    let themes = builtins();
    let pair = Some("dark:Gruvbox Dark,light:Gruvbox Light");
    assert_eq!(
        theme_choice_value(pair, Light, "Nord", &themes),
        ("dark:Gruvbox Dark,light:Nord".to_string(), Some(Light))
    );
    assert_eq!(
        theme_choice_value(pair, Dark, "Default", &themes),
        ("dark:Default,light:Gruvbox Light".to_string(), Some(Dark))
    );
    let unset = [None, Some(""), Some("  "), Some("dark:,light:")];
    for raw in unset {
        assert!(theme_is_unset(raw), "{raw:?}");
    }
    for raw in [Some("Dracula"), Some("dark:Dracula")] {
        assert!(!theme_is_unset(raw), "{raw:?}");
    }
    for raw in unset.into_iter().chain([Some("Dracula")]) {
        for theme in aterm_types::scheme::builtin_theme_list() {
            for side in [Dark, Light] {
                assert_eq!(
                    theme_choice_value(raw, side, theme.name, &themes),
                    (theme.name.to_string(), None),
                    "{raw:?}"
                );
            }
        }
    }
}

#[test]
fn a_one_theme_pair_takes_the_rules_every_pair_takes() {
    let themes = builtins();
    let same = Some("dark:Aterm Day,light:Aterm Day");
    assert_eq!(theme_mode(same), ThemeMode::FollowSystem);
    assert_eq!(
        theme_choice_value(same, Dark, "Linen", &themes),
        ("dark:Linen,light:Aterm Day".to_string(), Some(Dark))
    );
    assert_eq!(
        theme_sideless_choice_value(same, Dark, "Linen", &themes),
        ("Linen".to_string(), None)
    );
    assert_eq!(
        theme_sideless_choice_value(same, Dark, "Hearth", &themes),
        ("dark:Hearth,light:Aterm Day".to_string(), Some(Dark))
    );
    assert_eq!(
        theme_sideless_choice_value(same, Light, "Harbor Day", &themes).0,
        "dark:Aterm Day,light:Harbor Day"
    );
    let two = Some("dark:Nord,light:Aterm Day");
    assert_eq!(
        theme_choice_value(two, Dark, "Linen", &themes),
        ("dark:Linen,light:Aterm Day".to_string(), Some(Dark))
    );
    assert_eq!(
        theme_sideless_choice_value(two, Dark, "Linen", &themes),
        ("Linen".to_string(), None)
    );
}

#[test]
fn a_side_less_choice_never_files_a_theme_under_the_other_appearance() {
    let themes = builtins();
    let raws = [
        None,
        Some("Nord"),
        Some("dark:Default,light:Default Light"),
        Some("dark:Nord,light:Default Light"),
        Some("dark:Nord,light:Solarized Light"),
        Some("dark:Aterm Day,light:Aterm Day"),
    ];
    for raw in raws {
        for theme in aterm_types::scheme::builtin_theme_list() {
            for shown in [Dark, Light] {
                let (value, side) = theme_sideless_choice_value(raw, shown, theme.name, &themes);
                let (dark, light) = theme_slots(Some(&value), &themes);
                let drawn = match (theme_mode(Some(&value)), shown) {
                    (ThemeMode::Always, _) => value.clone(),
                    (ThemeMode::FollowSystem, Dark) => dark.clone(),
                    (ThemeMode::FollowSystem, Light) => light.clone(),
                };
                assert_eq!(drawn, theme.name, "{raw:?} {shown:?} -> {value}");
                if theme_mode(Some(&value)) == ThemeMode::FollowSystem {
                    assert_eq!(theme.appearance, shown, "{raw:?} -> {value}");
                    assert_eq!(side, Some(shown));
                    let (was_dark, was_light) = theme_slots(raw, &themes);
                    match shown {
                        Dark => assert_eq!(light, was_light, "{raw:?} -> {value}"),
                        Light => assert_eq!(dark, was_dark, "{raw:?} -> {value}"),
                    }
                } else {
                    assert_eq!(side, None);
                    assert_eq!(value, theme.name);
                }
            }
        }
    }
    assert_eq!(
        theme_sideless_choice_value(
            Some("dark:Nord,light:Default Light"),
            Dark,
            "Aterm Day",
            &themes
        ),
        ("Aterm Day".to_string(), None)
    );
    assert_eq!(
        theme_sideless_choice_value(
            Some("dark:Nord,light:Solarized Light"),
            Light,
            "Hearth",
            &themes
        ),
        ("Hearth".to_string(), None)
    );
    assert_eq!(
        theme_choice_value(
            Some("dark:Nord,light:Default Light"),
            Dark,
            "Aterm Day",
            &themes
        ),
        ("dark:Aterm Day,light:Default Light".to_string(), Some(Dark))
    );
}

#[test]
fn turning_follow_system_off_then_on_restores_the_pair_it_replaced() {
    let themes = builtins();
    let pair = "dark:Nord,light:Solarized Light";
    for (side, name) in [(Dark, "Nord"), (Light, "Solarized Light")] {
        let always = theme_always_value(Some(pair), side, &themes);
        assert_eq!(always, name);
        let kept = ThemeFollowKept {
            always: always.clone(),
            pair: pair.to_string(),
        };
        assert_eq!(
            theme_follow_system_value(Some(&always), Some(&kept), &themes),
            pair,
            "{side:?}"
        );
        assert_ne!(
            theme_follow_system_value(Some(&always), None, &themes),
            pair
        );
    }
    let kept = ThemeFollowKept {
        always: "Nord".to_string(),
        pair: pair.to_string(),
    };
    assert_eq!(
        theme_follow_system_value(Some("Dracula"), Some(&kept), &themes),
        theme_follow_system_value(Some("Dracula"), None, &themes)
    );
}

#[test]
fn a_slot_edit_keeps_the_other_side() {
    let themes = builtins();
    let raw = Some("dark:Gruvbox Dark,light:Gruvbox Light");
    assert_eq!(
        theme_with_side(raw, Light, "Solarized Light", &themes),
        "dark:Gruvbox Dark,light:Solarized Light"
    );
    assert_eq!(
        theme_with_side(raw, Dark, "Nord", &themes),
        "dark:Nord,light:Gruvbox Light"
    );
    assert_eq!(
        theme_with_side(
            Some("dark:Nord,light:Default Light"),
            Dark,
            "Default",
            &themes
        ),
        "dark:Default,light:Default Light"
    );
    assert_eq!(
        theme_with_side(None, Light, "GitHub Light", &themes),
        "dark:Default,light:GitHub Light"
    );
    let implicit = Some("dark:Gruvbox Dark");
    assert_eq!(
        theme_slots(implicit, &themes),
        ("Gruvbox Dark".to_string(), "Gruvbox Light".to_string())
    );
    assert_eq!(
        theme_with_side(implicit, Dark, "Nord", &themes),
        "dark:Nord,light:Gruvbox Light"
    );
    for theme in aterm_types::scheme::builtin_theme_list() {
        for side in [Dark, Light] {
            let written = theme_with_side(raw, side, theme.name, &themes);
            let (dark, light) = theme_slots(Some(&written), &themes);
            let (edited, kept, kept_was) = match side {
                Dark => (dark, light, "Gruvbox Light"),
                Light => (light, dark, "Gruvbox Dark"),
            };
            assert_eq!(edited, theme.name, "{written:?}");
            assert_eq!(kept, kept_was, "{written:?}");
        }
    }
}

#[test]
fn turning_follow_system_off_writes_the_visible_side() {
    let themes = builtins();
    let raw = Some("dark:Dracula,light:GitHub Light");
    assert_eq!(theme_always_value(raw, Dark, &themes), "Dracula");
    assert_eq!(theme_always_value(raw, Light, &themes), "GitHub Light");
    let on = theme_follow_system_value(Some("Gruvbox Dark"), None, &themes);
    assert_eq!(theme_always_value(Some(&on), Dark, &themes), "Gruvbox Dark");
}
