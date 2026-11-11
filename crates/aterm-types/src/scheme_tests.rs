// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn draw_builtin_follows_name_for() {
    let selectors = [
        "",
        "Nord",
        "nord",
        "dark:Gruvbox Dark",
        "light:Linen",
        "dark:Mine,light:Hearth",
        "Mine",
    ];
    for raw in selectors {
        let (selector, _) = ThemeSelector::parse(raw);
        for appearance in [Appearance::Dark, Appearance::Light] {
            let draw = selector.draw_builtin(appearance);
            let at = format!("{raw:?} on {appearance:?}");
            match selector.name_for(appearance) {
                None => assert_eq!(draw, SelectorDraw::Unset(ColorScheme::default()), "{at}"),
                Some(name) => match builtin(name) {
                    Some(scheme) => assert_eq!(draw, SelectorDraw::Builtin(scheme), "{at}"),
                    None => assert_eq!(draw, SelectorDraw::Other(name.to_string()), "{at}"),
                },
            }
        }
    }
    let draw = |raw: &str, a| ThemeSelector::parse(raw).0.draw_builtin(a);
    let builtin_named = |d: SelectorDraw| match d {
        SelectorDraw::Builtin(s) => s.name,
        other => panic!("expected a built-in, got {other:?}"),
    };
    assert_eq!(builtin_named(draw("nord", Appearance::Light)), "Nord");
    assert_eq!(
        builtin_named(draw("dark:Gruvbox Dark", Appearance::Light)),
        "Gruvbox Light"
    );
    assert_eq!(
        builtin_named(draw("dark:Mine,light:Hearth", Appearance::Light)),
        "Hearth"
    );
    assert!(matches!(draw("", Appearance::Light), SelectorDraw::Unset(s) if s.name == "Default"));
    assert_eq!(
        builtin_named(draw("dark:Default,light:Default Light", Appearance::Light)),
        "Default Light"
    );
    for a in [Appearance::Dark, Appearance::Light] {
        assert_eq!(draw("Mine", a), SelectorDraw::Other("Mine".into()));
    }
    assert_eq!(
        draw("dark:Mine,light:Hearth", Appearance::Dark),
        SelectorDraw::Other("Mine".into())
    );
}

#[test]
fn builtin_pairs_are_symmetric_and_cross_appearance() {
    for theme in builtin_theme_list() {
        let Some(pair) = theme.pair else { continue };
        let other = builtin_theme(pair).expect("a pair names a built-in");
        assert_eq!(
            other.name, pair,
            "{}: pair is spelled canonically",
            theme.name
        );
        assert_eq!(other.pair, Some(theme.name), "{} <-> {pair}", theme.name);
        assert_ne!(
            other.appearance, theme.appearance,
            "{} <-> {pair}",
            theme.name
        );
    }
    for (dark, light) in [
        ("Default", "Default Light"),
        ("Gruvbox Dark", "Gruvbox Light"),
        ("Solarized Dark", "Solarized Light"),
        ("Catppuccin Mocha", "Catppuccin Latte"),
    ] {
        assert_eq!(builtin_pair(dark), Some(light));
        assert_eq!(
            builtin_theme(dark).map(|t| t.appearance),
            Some(Appearance::Dark)
        );
    }
    assert_eq!(builtin_pair("gruvbox dark"), Some("Gruvbox Light"));
    assert_eq!(builtin_pair("Nord"), None);
    assert_eq!(builtin_pair("Studio Night"), None);
}

fn admitted(text: &str, id: &str, measured: Appearance) -> (ParsedTheme, ThemeMeta) {
    let mut parsed = parse_theme_str(text).expect("theme parses");
    let meta = ThemeMeta::admit(&mut parsed, id, measured);
    (parsed, meta)
}

#[test]
fn admission_takes_display_name_and_measured_appearance() {
    let clean = "foreground = #202020\nbackground = #f8f8f8\n";
    let (parsed, meta) = admitted(
        &format!("name = Studio Day\ndescription = calm\nauthor = Jo\n{clean}"),
        "studio",
        Appearance::Light,
    );
    assert_eq!(meta.display_name, "Studio Day");
    assert_eq!(meta.description.as_deref(), Some("calm"));
    assert_eq!(meta.author.as_deref(), Some("Jo"));
    assert_eq!(meta.appearance, Appearance::Light);
    assert_eq!(meta.appearance_source, AppearanceSource::Measured);
    assert_eq!(parsed.scheme.appearance, Appearance::Light, "written back");
    assert_eq!(meta.pair, None);
    assert!(meta.warnings.is_empty(), "{:?}", meta.warnings);

    let (_, meta) = admitted(clean, "studio", Appearance::Light);
    assert_eq!(meta.display_name, "studio", "no name => the id");

    let (parsed, meta) = admitted(
        &format!("appearance = dark\n{clean}"),
        "studio",
        Appearance::Light,
    );
    assert_eq!(meta.appearance, Appearance::Dark);
    assert_eq!(meta.appearance_source, AppearanceSource::Declared);
    assert_eq!(
        parsed.scheme.appearance,
        Appearance::Dark,
        "declared is kept"
    );
    assert_eq!(
        meta.warnings,
        vec![ThemeWarning::AppearanceMismatch {
            declared: Appearance::Dark,
            measured: Appearance::Light,
        }]
    );
}

#[test]
fn admission_warnings_name_each_defect() {
    let base = ColorScheme {
        name: String::new(),
        appearance: Appearance::Dark,
        foreground: Rgb::new(0xd0, 0xd0, 0xd0),
        background: Rgb::new(0x10, 0x10, 0x10),
        cursor: Some(Rgb::new(0x50, 0xfa, 0x7b)),
        selection: Some(Rgb::new(0x40, 0x48, 0x60)),
        selection_fg: None,
        ansi: ColorScheme::default().ansi,
        chrome: ChromeColors::default(),
        palette_generate: false,
    };
    assert_eq!(admission_warnings(&base, Appearance::Dark), vec![]);
    let kinds = |scheme: &ColorScheme| {
        admission_warnings(scheme, Appearance::Dark)
            .iter()
            .map(core::mem::discriminant)
            .collect::<Vec<_>>()
    };
    let mut s = base.clone();
    s.foreground = Rgb::new(0x30, 0x30, 0x30);
    assert_eq!(
        kinds(&s),
        vec![core::mem::discriminant(&ThemeWarning::LowTextContrast {
            hundredths: 0
        })]
    );
    let mut s = base.clone();
    s.selection = Some(Rgb::new(0x18, 0x18, 0x18));
    assert_eq!(
        kinds(&s),
        vec![core::mem::discriminant(&ThemeWarning::FaintSelection {
            hundredths: 0
        })]
    );
    let mut s = base.clone();
    s.ansi[0] = s.background;
    s.ansi[8] = s.background;
    assert_eq!(
        admission_warnings(&s, Appearance::Dark),
        vec![
            ThemeWarning::AnsiMatchesBackground { slot: 0 },
            ThemeWarning::AnsiMatchesBackground { slot: 8 },
        ]
    );
    let mut s = base.clone();
    s.cursor = Some(Rgb::new(0x20, 0x20, 0x20));
    assert_eq!(
        kinds(&s),
        vec![core::mem::discriminant(&ThemeWarning::LowCursorContrast {
            hundredths: 0
        })]
    );
    let mut s = base.clone();
    s.cursor = None;
    assert_eq!(kinds(&s), vec![]);
    for (key, floor) in [
        (ThemeKey::AccentColor, 300),
        (ThemeKey::BorderColor, 150),
        (ThemeKey::SearchMatchColor, 150),
    ] {
        let mut s = base.clone();
        *s.chrome.slot_mut(key).expect("a chrome key") = Some(s.background);
        assert_eq!(
            admission_warnings(&s, Appearance::Dark),
            vec![ThemeWarning::ChromeBelowFloor {
                key,
                hundredths: 100,
                floor_hundredths: floor,
            }],
            "{key:?}"
        );
    }
    let mut s = base.clone();
    s.chrome.search_match = s.selection;
    assert_eq!(
        admission_warnings(&s, Appearance::Dark),
        vec![ThemeWarning::SearchMatchLikeSelection]
    );
    let mut s = base.clone();
    s.chrome = ChromeColors {
        accent: Some(Rgb::new(0x7a, 0xa2, 0xf7)),
        active_tab: Some(Rgb::new(0x10, 0x10, 0x10)),
        border: Some(Rgb::new(0x50, 0x50, 0x50)),
        search_match: Some(Rgb::new(0x3d, 0x6f, 0x2f)),
    };
    assert_eq!(admission_warnings(&s, Appearance::Dark), vec![]);
    let mut s = base;
    s.foreground = Rgb::new(0x30, 0x30, 0x30);
    let warning = admission_warnings(&s, Appearance::Dark)[0];
    let ThemeWarning::LowTextContrast { hundredths } = warning else {
        panic!("{warning:?}");
    };
    let exact = s.foreground.contrast(s.background);
    assert!((f64::from(hundredths) / 100.0 - exact).abs() < 0.01);
    assert!(warning.to_string().contains(&format!(
        "{}.{:02}:1",
        hundredths / 100,
        hundredths % 100
    )));
}

#[test]
fn builtin_meta_is_its_identity() {
    let theme = builtin_theme("default light").expect("Default Light is a built-in");
    let meta = ThemeMeta::of_builtin(theme);
    assert_eq!(meta.display_name, "Default Light");
    assert_eq!(meta.appearance, Appearance::Light);
    assert_eq!(meta.appearance_source, AppearanceSource::Declared);
    assert_eq!(meta.pair.as_deref(), Some("Default"));
    assert!(meta.warnings.is_empty());
}

#[test]
fn user_theme_name_grammar_rejects_paths_splits_and_normalization_traps() {
    for unsafe_name in [
        "",
        ".",
        "..",
        " ../Nord",
        "../Nord",
        "/tmp/Nord",
        r"C:\\tmp\\Nord",
        "Nord ",
        "Nord\nDark",
        "dark:Foo",
        "LIGHT:Foo",
        "Foo,Bar",
        "Foo[1]",
        "]",
        " dark :Foo",
    ] {
        assert!(
            validate_user_theme_name(unsafe_name).is_err(),
            "unsafe theme name accepted: {unsafe_name:?}"
        );
    }
    for safe_name in ["Nord", "My Theme", "Tokyo-Night_2", "work:blue"] {
        assert_eq!(validate_user_theme_name(safe_name), Ok(()));
    }
}

#[test]
fn theme_selector_table() {
    use Appearance::{Dark, Light};
    use SelectorIssue as I;
    type Case<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [I]);
    let cases: &[Case<'_>] = &[
        ("Nord", Some("Nord"), Some("Nord"), &[]),
        (
            "  Tokyo Night ",
            Some("Tokyo Night"),
            Some("Tokyo Night"),
            &[],
        ),
        ("work:blue", Some("work:blue"), Some("work:blue"), &[]),
        ("", None, None, &[I::EmptyName(None)]),
        ("   ", None, None, &[I::EmptyName(None)]),
        (
            "dark:Nord,light:GitHub Light",
            Some("Nord"),
            Some("GitHub Light"),
            &[],
        ),
        (
            " DARK : Nord , Light : Day ",
            Some("Nord"),
            Some("Day"),
            &[],
        ),
        ("dark:Nord", Some("Nord"), Some("Nord"), &[]),
        ("light:Day", Some("Day"), Some("Day"), &[]),
        (
            "dark:Gruvbox Dark",
            Some("Gruvbox Dark"),
            Some("Gruvbox Light"),
            &[],
        ),
        (
            "light:catppuccin latte",
            Some("Catppuccin Mocha"),
            Some("catppuccin latte"),
            &[],
        ),
        ("dark:Default", Some("Default"), Some("Default Light"), &[]),
        (
            "dark:A,light:,LIGHT:x",
            Some("A"),
            Some("x"),
            &[I::EmptyName(Some(Light))],
        ),
        (
            "dark:,light:Solarized Light",
            Some("Solarized Dark"),
            Some("Solarized Light"),
            &[I::EmptyName(Some(Dark))],
        ),
        (
            "dark:A,dark:B",
            Some("A"),
            Some("A"),
            &[I::DuplicateSide(Dark)],
        ),
        (
            "dark:A,light:L,LIGHT:M",
            Some("A"),
            Some("L"),
            &[I::DuplicateSide(Light)],
        ),
        (
            "dark:Nord,lite:Work",
            Some("Nord"),
            Some("Nord"),
            &[I::UnknownMode(String::new())],
        ),
        (
            "dark:Nord,sepia",
            Some("Nord"),
            Some("Nord"),
            &[I::JunkSegment(String::new())],
        ),
        (
            "dark:Nord,",
            Some("Nord"),
            Some("Nord"),
            &[I::JunkSegment(String::new())],
        ),
        (
            "dark:,light:",
            None,
            None,
            &[I::EmptyName(Some(Dark)), I::EmptyName(Some(Light))],
        ),
        (
            "dark:,light:Day",
            Some("Day"),
            Some("Day"),
            &[I::EmptyName(Some(Dark))],
        ),
    ];
    for (raw, dark, light, kinds) in cases {
        let (selector, issues) = ThemeSelector::parse(raw);
        assert_eq!(selector.name_for(Dark), *dark, "{raw:?} dark");
        assert_eq!(selector.name_for(Light), *light, "{raw:?} light");
        assert_eq!(
            issues
                .iter()
                .map(core::mem::discriminant)
                .collect::<Vec<_>>(),
            kinds
                .iter()
                .map(core::mem::discriminant)
                .collect::<Vec<_>>(),
            "{raw:?} issue kinds: {issues:?}"
        );
        for (issue, kind) in issues.iter().zip(kinds.iter()) {
            match (issue, kind) {
                (I::DuplicateSide(a), I::DuplicateSide(b))
                | (I::EmptyName(Some(a)), I::EmptyName(Some(b))) => {
                    assert_eq!(a, b, "{raw:?}");
                }
                (I::EmptyName(a), I::EmptyName(b)) => assert_eq!(a, b, "{raw:?}"),
                _ => {}
            }
        }
    }
}

#[test]
fn admitted_names_parse_as_themselves() {
    for name in [
        "Nord",
        "My Theme",
        "work:blue",
        "Tokyo-Night_2",
        "a:b:c",
        "darkish:x",
    ] {
        assert_eq!(validate_user_theme_name(name), Ok(()), "{name:?}");
        let (selector, issues) = ThemeSelector::parse(name);
        assert!(issues.is_empty(), "{name:?}: {issues:?}");
        assert_eq!(selector, ThemeSelector::Single(name.to_string()));
    }
    assert!(validate_user_theme_name("dark:Nord").is_err());
    assert!(matches!(
        ThemeSelector::parse("dark:Nord").0,
        ThemeSelector::Split { .. }
    ));
}

#[test]
fn default_scheme_matches_palette_defaults() {
    let s = ColorScheme::default();
    let p = s.to_color_palette();
    for i in 0u16..=255 {
        let idx = i as u8;
        assert_eq!(p.get(idx), ColorPalette::default_color(idx), "index {idx}");
    }
}

#[test]
fn builtin_lookup() {
    assert_eq!(builtin("default"), Some(ColorScheme::default()));
    assert_eq!(builtin("DEFAULT"), Some(ColorScheme::default()));
    assert!(builtin("no-such-theme-xyz").is_none());
    assert_eq!(builtin_names()[0], "Default");
}

#[test]
fn builtins_are_legible() {
    for theme in builtin_theme_list() {
        let name = theme.name;
        let s = builtin(name).expect("a listed built-in resolves");
        let bg = s.background;
        let text = s.foreground.contrast(bg);
        assert!(text >= 4.5, "{name}: text {text:.2}:1 < 4.5:1");
        let band = s.selection_band().contrast(bg);
        assert!(band >= 1.5, "{name}: selection band {band:.2}:1 < 1.5:1");
        let cursor = s.cursor.unwrap_or(s.foreground).contrast(bg);
        assert!(cursor >= 3.0, "{name}: cursor {cursor:.2}:1 < 3:1");
        for (slot, colour) in s.ansi.iter().enumerate() {
            assert_ne!(*colour, bg, "{name}: color{slot} is the background");
        }
        assert_eq!(
            admission_warnings(&s, s.appearance),
            [],
            "{name} would be admitted with warnings"
        );
    }
}

#[test]
fn signature_themes_meet_their_bars() {
    let signature: Vec<BuiltinTheme> = builtin_theme_list()
        .into_iter()
        .filter(|t| t.signature)
        .collect();
    assert_eq!(signature.len(), 4, "{signature:?}");
    for theme in &signature {
        let name = theme.name;
        let s = builtin(name).expect("a listed built-in resolves");
        assert_eq!(crate::scheme_signature::signature_report(&s), [], "{name}");
        let pair = theme
            .pair
            .and_then(builtin_theme)
            .expect("a signature pair");
        assert!(pair.signature, "{name} pairs with {}", pair.name);
        assert_ne!(
            pair.appearance, theme.appearance,
            "{name} <-> {}",
            pair.name
        );
    }
    assert!(!crate::scheme_signature::signature_report(&ColorScheme::default()).is_empty());
}

#[test]
fn signature_themes_list_right_after_default() {
    let list = builtin_theme_list();
    assert_eq!(list[0].name, "Default");
    let first_other = list[1..]
        .iter()
        .position(|t| !t.signature)
        .expect("ported themes follow");
    assert!(first_other > 0, "a signature theme follows Default");
    assert!(
        list[1 + first_other..].iter().all(|t| !t.signature),
        "{list:?}"
    );
    assert_eq!(
        builtin_names()[1..5],
        ["Aterm Night", "Aterm Day", "Hearth", "Linen"]
    );
}

#[test]
fn an_unset_selection_follows_the_theme() {
    let light = parse_scheme_str("foreground = #24292f\nbackground = #fafafa\n")
        .expect("a minimal light theme parses");
    assert_eq!(light.selection, None);
    let (fg, bg) = (light.foreground, light.background);
    let band = light.selection_band();
    assert!(band.contrast(bg) < band.contrast(fg), "{band:?}");
    assert!(band.contrast(bg) >= MIN_SELECTION_CONTRAST, "{band:?}");
    assert!(fg.contrast(band) >= 4.5, "{band:?}");
    assert_eq!(
        Rgb::new(
            (light.to_theme_parts().selection >> 16) as u8,
            (light.to_theme_parts().selection >> 8) as u8,
            light.to_theme_parts().selection as u8
        ),
        band,
        "the renderer paints the same band"
    );
    assert!(
        SELECTION_DEFAULT.contrast(bg) > SELECTION_DEFAULT.contrast(fg),
        "control: the dark default is on the light theme's text side"
    );
    assert!(
        !admission_warnings(&light, Appearance::Light)
            .iter()
            .any(|w| matches!(w, ThemeWarning::FaintSelection { .. })),
        "the derived band is not faint"
    );

    let dark = parse_scheme_str("foreground = #d0d0d0\nbackground = #101418\n")
        .expect("a minimal dark theme parses");
    let band = dark.selection_band();
    assert!(band.contrast(dark.background) < band.contrast(dark.foreground));
    assert!(band.contrast(dark.background) >= MIN_SELECTION_CONTRAST);
    let set =
        parse_scheme_str("fg = #000000\nbg = #ffffff\nselection = #123456\n").expect("parses");
    assert_eq!(set.selection_band(), Rgb::new(0x12, 0x34, 0x56));
}

#[test]
fn derived_selection_is_the_rounded_blend() {
    let black = Rgb::new(0, 0, 0);
    let white = Rgb::new(255, 255, 255);
    assert_eq!(derived_selection(white, black), Rgb::new(56, 56, 56));
    assert_eq!(derived_selection(black, white), Rgb::new(199, 199, 199));
    let grey = Rgb::new(0x80, 0x80, 0x80);
    assert_eq!(derived_selection(grey, grey), grey);
}

#[test]
fn light_builtins_are_light_and_selection_legible() {
    let luma = |c: Rgb| 0.299 * f64::from(c.r) + 0.587 * f64::from(c.g) + 0.114 * f64::from(c.b);
    let mut saw_light = 0;
    for name in builtin_names() {
        let s = builtin(name).expect("builtin resolves");
        if s.appearance == Appearance::Light {
            saw_light += 1;
            assert!(
                luma(s.background) > luma(s.foreground),
                "{name}: a Light scheme must have a background brighter than its fg"
            );
        }
        let sel = s.selection_band();
        let contrast = s.foreground.contrast(sel);
        assert!(
            contrast >= 3.0,
            "{name}: fg-over-selection contrast {contrast:.2} < 3.0:1 (selected text would be illegible)"
        );
    }
    assert!(
        saw_light >= 4,
        "expected the bundled light schemes to be present"
    );
}

#[test]
fn parse_scheme_str_full_theme() {
    let src = "\
# a sample user theme (Tokyo Night-ish)
name = My Night
appearance = dark
foreground = #c0caf5
background = #1a1b26
cursor = #7aa2f7
selection = #283457
color0  = #15161e
color1  = #f7768e
palette2 = #9ece6a   # alias `paletteN`
ansi3    = #e0af68   ; alias `ansiN`
color4  = #7aa2f7
color5  = #bb9af7
color6  = #7dcfff
color7  = #a9b1d6
color8  = #414868
color9  = #ff899d
color10 = #9fe044
color11 = #faba4a
color12 = #8db0ff
color13 = #c7a9ff
color14 = #a4daff
color15 = #c0caf5
";
    let s = parse_scheme_str(src).expect("valid theme parses");
    assert_eq!(s.name, "My Night");
    assert_eq!(s.appearance, Appearance::Dark);
    assert_eq!(s.foreground, Rgb::new(0xc0, 0xca, 0xf5));
    assert_eq!(s.background, Rgb::new(0x1a, 0x1b, 0x26));
    assert_eq!(s.cursor, Some(Rgb::new(0x7a, 0xa2, 0xf7)));
    assert_eq!(s.selection, Some(Rgb::new(0x28, 0x34, 0x57)));
    assert_eq!(s.ansi[0], Rgb::new(0x15, 0x16, 0x1e));
    assert_eq!(s.ansi[1], Rgb::new(0xf7, 0x76, 0x8e));
    assert_eq!(s.ansi[2], Rgb::new(0x9e, 0xce, 0x6a));
    assert_eq!(s.ansi[3], Rgb::new(0xe0, 0xaf, 0x68));
    assert_eq!(s.ansi[15], Rgb::new(0xc0, 0xca, 0xf5));
    assert_eq!(s.to_theme_parts().bg, 0x001a_1b26);
    assert_eq!(s.to_color_palette().get(1), Rgb::new(0xf7, 0x76, 0x8e));
}

#[test]
fn parse_scheme_str_minimal_defaults_unset_slots() {
    let s = parse_scheme_str("foreground = #ffffff\nbackground = #000000\n")
        .expect("minimal theme parses");
    assert_eq!(s.foreground, Rgb::new(0xff, 0xff, 0xff));
    assert_eq!(s.background, Rgb::new(0, 0, 0));
    assert_eq!(s.cursor, None);
    assert_eq!(s.selection, None);
    for i in 0u8..16 {
        assert_eq!(
            s.ansi[i as usize],
            ColorPalette::default_color(i),
            "slot {i}"
        );
    }
    let s2 = parse_scheme_str("fg = #abcdef\nbg = #123456\n").unwrap();
    assert_eq!(s2.foreground, Rgb::new(0xab, 0xcd, 0xef));
    assert_eq!(s2.background, Rgb::new(0x12, 0x34, 0x56));
}

#[test]
fn palette_generate_key_parses() {
    let base = "foreground = #d0d0d0\nbackground = #101010\n";
    let read = |line: &str| parse_scheme_str(&format!("{base}{line}\n"));
    assert!(read("palette_generate = true").unwrap().palette_generate);
    assert!(read("palette_generate = TRUE").unwrap().palette_generate);
    assert!(!read("palette_generate = false").unwrap().palette_generate);
    assert!(!parse_scheme_str(base).unwrap().palette_generate);
    let e = read("palette_generate = yes").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 3, .. }), "{e:?}");
    let e = read("palette_generate = true\npalette_generate = false").unwrap_err();
    assert!(
        matches!(&e, ThemeError::Parse { line: 4, reason, .. } if reason.contains("duplicate")),
        "{e:?}"
    );
}

#[test]
fn parse_scheme_str_rejects_malformed() {
    let e = parse_scheme_str("foreground = #fff000\nnonsense\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
    let e = parse_scheme_str("foreground = #zzzzzz\nbackground = #000000\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 1, .. }), "{e:?}");
    let e = parse_scheme_str("foreground = #fff000\nbackground=#000000\nwidth = 80\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 3, .. }), "{e:?}");
    let e =
        parse_scheme_str("foreground=#ffffff\nbackground=#000000\ncolor16=#111111\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { .. }), "{e:?}");
    let e = parse_scheme_str("fg=#ffff\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 1, .. }), "{e:?}");
    let e = parse_scheme_str(
        "foreground=#ffffff\nbackground=#000000\ncolor1=#111111\ncolor1=#222222\n",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ThemeError::Parse { reason, .. } if reason.contains("duplicate")),
        "{e:?}"
    );
    let e = parse_scheme_str("foreground = #ffffff\n").unwrap_err();
    assert!(
        matches!(&e, ThemeError::Parse { reason, .. } if reason.contains("background")),
        "{e:?}"
    );
}

#[test]
fn parse_theme_ignores_a_leading_byte_order_mark() {
    let plain = "foreground = #ffffff\nbackground = #000000\n";
    let marked = format!("\u{feff}{plain}");
    assert_eq!(parse_theme_str(&marked), parse_theme_str(plain));
    let e = parse_theme_str("\u{feff}foreground = #ffffff\nbogus = 1\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
    let e = parse_theme_str("foreground = #ffffff\n\u{feff}background = #000000\n").unwrap_err();
    assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
}

#[test]
fn parse_theme_duplicate_keys_fold_aliases() {
    let e =
        parse_scheme_str("foreground = #ffffff\nfg = #000000\nbackground = #000000\n").unwrap_err();
    assert!(
        matches!(&e, ThemeError::Parse { line: 2, reason, .. }
            if reason.contains("duplicate") && reason.contains("line 1")),
        "{e:?}"
    );
    let groups: &[&[(&str, &str)]] = &[
        &[("name", "A")],
        &[("description", "A")],
        &[("author", "A")],
        &[("appearance", "dark")],
        &[("foreground", "#111111"), ("fg", "#111111")],
        &[("background", "#111111"), ("bg", "#111111")],
        &[("cursor", "#111111"), ("cursor_color", "#111111")],
        &[("selection", "#111111"), ("selection_color", "#111111")],
        &[("selection_foreground", "#111111")],
        &[
            ("color1", "#111111"),
            ("palette1", "#111111"),
            ("ansi1", "#111111"),
        ],
    ];
    for group in groups {
        for (k1, v1) in *group {
            for (k2, v2) in *group {
                let src = format!("{k1} = {v1}\n{} = {v2}\n", k2.to_ascii_uppercase());
                let e = parse_scheme_str(&src).unwrap_err();
                assert!(
                    matches!(&e, ThemeError::Parse { line: 2, reason, .. }
                        if reason.contains("duplicate")),
                    "{src:?}: {e:?}"
                );
            }
        }
    }
    let once = "name = A\ndescription = B\nauthor = C\nappearance = light\nfg = #111111\n\
                bg = #eeeeee\ncursor_color = #222222\nselection_color = #333333\n\
                selection_foreground = #444444\npalette1 = #555555\nansi2 = #666666\n";
    let parsed = parse_theme_str(once).expect("one of each parses");
    assert_eq!(parsed.description.as_deref(), Some("B"));
    assert_eq!(parsed.author.as_deref(), Some("C"));
    assert!(parsed.appearance_declared);
    assert_eq!(parsed.scheme.appearance, Appearance::Light);
    assert_eq!(parsed.scheme.ansi[1], Rgb::new(0x55, 0x55, 0x55));
    assert_eq!(parsed.scheme.ansi[2], Rgb::new(0x66, 0x66, 0x66));
}

#[test]
fn parse_theme_dtp_spellings_equal_short_ones() {
    let base = "foreground = #c0caf5\nbackground = #1a1b26\n";
    let short = parse_scheme_str(&format!("{base}cursor = #7aa2f7\nselection = #283457\n"))
        .expect("short spellings parse");
    let long = parse_scheme_str(&format!(
        "{base}cursor_color = #7aa2f7\nselection_color = #283457\n"
    ))
    .expect("aterm.toml spellings parse");
    assert_eq!(short, long);
    assert_eq!(long.cursor, Some(Rgb::new(0x7a, 0xa2, 0xf7)));
    assert_eq!(long.selection, Some(Rgb::new(0x28, 0x34, 0x57)));
    assert_eq!(long.selection_fg, None);
    let with_fg = parse_scheme_str(&format!("{base}selection_foreground = #ffffff\n"))
        .expect("selection_foreground parses");
    assert_eq!(with_fg.selection_fg, Some(Rgb::new(0xff, 0xff, 0xff)));
    assert_eq!(ColorScheme::default().selection_fg, None);
    for name in builtin_names() {
        assert_eq!(builtin(name).unwrap().selection_fg, None, "{name}");
    }
}

#[test]
fn parse_hex_short_form_widens_each_digit() {
    assert_eq!(parse_hex("#abc"), parse_hex("#aabbcc"));
    assert_eq!(parse_hex("#ABC"), Some(Rgb::new(0xaa, 0xbb, 0xcc)));
    for d in "0123456789abcdef".chars() {
        let short: String = [d; 3].iter().collect();
        let long: String = [d; 6].iter().collect();
        assert_eq!(
            parse_hex(&format!("#{short}")),
            parse_hex(&format!("#{long}")),
            "{d}"
        );
        assert!(parse_hex(&format!("#{short}")).is_some());
        assert_eq!(parse_hex(&long), parse_hex(&format!("#{long}")), "{d}");
    }
    for bad in [
        "", "#", "#a", "#ab", "#abcd", "#abcde", "#abcdefa", "#ggg", "#é12", "bad", "fed", "abc",
    ] {
        assert_eq!(parse_hex(bad), None, "{bad:?}");
    }
    let s = parse_scheme_str("fg = #abc\nbg = #000\n").expect("#RGB colours parse");
    assert_eq!(s.foreground, Rgb::new(0xaa, 0xbb, 0xcc));
    assert_eq!(s.background, Rgb::new(0, 0, 0));
}

#[test]
fn parse_theme_text_fields_are_bounded() {
    let base = "fg = #ffffff\nbg = #000000\n";
    for key in ["name", "description", "author"] {
        let at_cap = "é".repeat(MAX_THEME_TEXT_CHARS);
        let parsed = parse_theme_str(&format!("{base}{key} = {at_cap}\n"))
            .unwrap_or_else(|e| panic!("{key} at the cap parses: {e}"));
        let got = match key {
            "name" => Some(parsed.scheme.name),
            "description" => parsed.description,
            _ => parsed.author,
        };
        assert_eq!(got.as_deref(), Some(at_cap.as_str()), "{key}");
        let over = "x".repeat(MAX_THEME_TEXT_CHARS + 1);
        let e = parse_theme_str(&format!("{base}{key} = {over}\n")).unwrap_err();
        assert!(
            matches!(e, ThemeError::Parse { line: 3, .. }),
            "{key}: {e:?}"
        );
        let e = parse_theme_str(&format!("{base}{key} = a\u{7}b\n")).unwrap_err();
        assert!(
            matches!(e, ThemeError::Parse { line: 3, .. }),
            "{key}: {e:?}"
        );
    }
    let plain = parse_theme_str(base).unwrap();
    assert_eq!((plain.description, plain.author), (None, None));
    assert!(!plain.appearance_declared);
}

#[test]
fn parse_theme_foreign_formats_are_named() {
    let cases: &[(&str, ForeignFormat)] = &[
        (
            "# kitty theme\nforeground #c0caf5\nbackground #1a1b26\ncolor0 #15161e\n",
            ForeignFormat::Kitty,
        ),
        (
            "background = 1a1b26\nforeground = c0caf5\npalette = 0=#15161e\n",
            ForeignFormat::Ghostty,
        ),
        (
            "foreground = #c0caf5\nbackground = #1a1b26\ncursor-color = #7aa2f7\n",
            ForeignFormat::Ghostty,
        ),
        (
            "[colors.primary]\nbackground = '#1a1b26'\nforeground = '#c0caf5'\n",
            ForeignFormat::Toml,
        ),
        (
            "foreground = \"#c0caf5\"\nbackground = \"#1a1b26\"\n",
            ForeignFormat::Toml,
        ),
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict/>\n</plist>\n",
            ForeignFormat::Plist,
        ),
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             <key>TextColor</key>\n<data>YnBsaXN0MDA=</data>\n</dict>\n</plist>\n",
            ForeignFormat::TerminalApp,
        ),
        (
            "[colors]\nforeground = \"#c0caf5\"\nansi = [\"#15161e\"]\n",
            ForeignFormat::WezTerm,
        ),
        (
            "{\n  \"name\": \"Night\",\n  \"background\": \"#1a1b26\"\n}\n",
            ForeignFormat::Json,
        ),
    ];
    for (src, want) in cases {
        assert_eq!(sniff_foreign_format(src), Some(*want), "{src:?}");
        let e = parse_scheme_str(src).unwrap_err();
        assert!(
            matches!(&e, ThemeError::Parse { foreign_format: Some(got), .. } if got == want),
            "{src:?}: {e:?}"
        );
        assert!(e.to_string().contains("aterm theme import"), "{e}");
    }
    for native in [
        "foreground = #c0caf5\nbackground = #1a1b26\nwidth = 80\n",
        "foreground = #zzzzzz\nbackground = #1a1b26\n",
        "name = Tokyo # night\nfg = #fff\nbg = #000\nfg = #111\n",
        "foreground = #c0caf5\n",
    ] {
        assert_eq!(sniff_foreign_format(native), None, "{native:?}");
        let e = parse_scheme_str(native).unwrap_err();
        assert!(
            matches!(
                &e,
                ThemeError::Parse {
                    foreign_format: None,
                    ..
                }
            ),
            "{native:?}: {e:?}"
        );
        assert!(!e.to_string().contains("aterm theme import"), "{e}");
    }
}

#[test]
fn parse_theme_unknown_keys_stay_fatal() {
    let base = "foreground = #c0caf5\nbackground = #1a1b26\n";
    for key in [
        "width",
        "cursor-color",
        "selection-background",
        "selection_background",
        "cursor_text_color",
        "palette",
        "color16",
        "font_px",
        "font_size",
    ] {
        let e = parse_scheme_str(&format!("{base}{key} = #123456\n")).unwrap_err();
        assert!(
            matches!(&e, ThemeError::Parse { line: 3, reason, .. } if reason.contains("unknown key")),
            "{key}: {e:?}"
        );
    }
}

#[test]
fn no_theme_key_spelling_names_two_keys() {
    let mut spellings = std::collections::HashSet::new();
    for spec in &THEME_KEYS {
        for spelling in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
            assert!(spellings.insert(spelling), "{spelling} is two keys");
        }
    }
}

fn file_from_examples(skip: Option<ThemeKey>, alias: bool) -> String {
    let mut text = String::new();
    for spec in THEME_KEYS.iter().filter(|spec| Some(spec.key) != skip) {
        let name = if alias {
            spec.aliases.first().copied().unwrap_or(spec.name)
        } else {
            spec.name
        };
        let mut line = |key: String| {
            text.push_str(&key);
            text.push_str(" = ");
            text.push_str(spec.example);
            text.push('\n');
        };
        if spec.indexed == 0 {
            line(name.to_string());
        } else {
            for slot in 0..spec.indexed {
                line(format!("{name}{slot}"));
            }
        }
    }
    text
}

#[test]
fn theme_key_table_drives_the_parser() {
    let canonical = parse_theme_str(&file_from_examples(None, false)).expect("examples parse");
    let aliased = parse_theme_str(&file_from_examples(None, true)).expect("aliases parse");
    assert_eq!(canonical, aliased);
    let example = |key| parse_hex(theme_key_spec(key).example).expect("a colour example");
    assert_eq!(canonical.scheme.foreground, example(ThemeKey::Foreground));
    assert_eq!(canonical.scheme.ansi[15], example(ThemeKey::Palette));
    assert!(canonical.appearance_declared);
    for spec in &THEME_KEYS {
        if spec.value == ThemeValueType::Colour {
            assert!(parse_hex(spec.example).is_some(), "{}", spec.name);
        }
        let without = parse_theme_str(&file_from_examples(Some(spec.key), false));
        if spec.required {
            let Err(ThemeError::Parse { line, key, .. }) = without else {
                panic!("{} is required: {without:?}", spec.name);
            };
            assert_eq!((line, key.as_deref()), (0, Some(spec.name)));
        } else {
            assert!(without.is_ok(), "{} is optional: {without:?}", spec.name);
        }
    }
    let mut extra = file_from_examples(None, false);
    extra.push_str("colour16 = #000000\n");
    assert!(matches!(
        parse_theme_str(&extra),
        Err(ThemeError::Parse { key: Some(ref k), .. }) if k == "colour16"
    ));
    let chrome = canonical.scheme.chrome;
    for (key, slot) in [
        (ThemeKey::AccentColor, chrome.accent),
        (ThemeKey::ActiveTabColor, chrome.active_tab),
        (ThemeKey::BorderColor, chrome.border),
        (ThemeKey::SearchMatchColor, chrome.search_match),
    ] {
        assert_eq!(slot, Some(example(key)), "{key:?}");
        assert_eq!(chrome.slot(key), Some(slot));
        assert_eq!(theme_key_spec(key).group, ThemeKeyGroup::Chrome);
    }
    for written in [
        crate::scheme_import::to_native_conf(&canonical.scheme, true, None, None, &[]),
        crate::scheme_import::to_native_conf_commented(&canonical.scheme, true, None, None, &[]),
    ] {
        let back = parse_theme_str(&written).expect("the writer's file parses");
        assert_eq!(back.scheme, canonical.scheme, "{written}");
    }
    let mut twice = file_from_examples(None, false);
    let first = twice
        .lines()
        .position(|l| l.starts_with("accent_color"))
        .expect("set")
        + 1;
    twice.push_str("accent_color = #ffffff\n");
    let Err(ThemeError::Parse { reason, key, .. }) = parse_theme_str(&twice) else {
        panic!("a duplicate accent_color is refused");
    };
    assert_eq!(key.as_deref(), Some("accent_color"));
    assert!(reason.contains(&format!("line {first}")), "{reason}");
    let mut near = file_from_examples(None, false);
    near.push_str("accent = #ffffff\n");
    assert!(matches!(
        parse_theme_str(&near),
        Err(ThemeError::Parse { key: Some(ref k), .. }) if k == "accent"
    ));
    assert_eq!(lookup_theme_key("color+1"), None);
    assert_eq!(lookup_theme_key("color16"), None);
    assert_eq!(
        lookup_theme_key("ANSI15").map(|(_, slot)| slot),
        Some(Some(15))
    );
}

#[test]
fn parse_theme_located_points_at_each_key() {
    let text = "# c\nfg = #ffffff\n\nbackground = #000000\npalette3 = #123\n";
    let (_, lines) = parse_theme_located(text).expect("parses");
    assert_eq!(lines.line(ThemeKey::Foreground, None), Some(2));
    assert_eq!(lines.line(ThemeKey::Background, None), Some(4));
    assert_eq!(lines.line(ThemeKey::Palette, Some(3)), Some(5));
    assert_eq!(lines.line(ThemeKey::Palette, Some(4)), None);
    assert_eq!(lines.line(ThemeKey::Cursor, None), None);
    let Err(ThemeError::Parse { line, key, .. }) =
        parse_theme_located("fg = #fff\nbg = #000\ncolor2 = nope\n")
    else {
        panic!("a bad colour is refused");
    };
    assert_eq!((line, key.as_deref()), (3, Some("color2")));
}

#[test]
fn contrast_warnings_are_the_report_rows_below_their_bar() {
    let mut schemes: Vec<ColorScheme> = builtin_names().into_iter().filter_map(builtin).collect();
    for grey in (0u8..=255).step_by(17) {
        schemes.push(ColorScheme {
            foreground: Rgb::new(grey, grey, grey),
            selection: Some(Rgb::new(grey / 2, grey / 2, grey)),
            cursor: (grey % 2 == 0).then_some(Rgb::new(grey, 0x20, 0x20)),
            chrome: ChromeColors {
                accent: (grey % 3 == 0).then_some(Rgb::new(grey, grey / 2, 0x40)),
                border: (grey % 3 == 1).then_some(Rgb::new(grey, grey, grey)),
                search_match: (grey % 3 == 2).then_some(Rgb::new(0x20, grey, 0x20)),
                active_tab: None,
            },
            ..ColorScheme::default()
        });
    }
    let mut warned = 0;
    for scheme in &schemes {
        let report = contrast_report(scheme);
        let from_rows: Vec<ThemeWarning> = report
            .iter()
            .filter(|row| row.below_minimum())
            .map(|row| match row.pair {
                ContrastPair::TextOnBackground => ThemeWarning::LowTextContrast {
                    hundredths: row.hundredths,
                },
                ContrastPair::SelectionOnBackground => ThemeWarning::FaintSelection {
                    hundredths: row.hundredths,
                },
                ContrastPair::CursorOnBackground => ThemeWarning::LowCursorContrast {
                    hundredths: row.hundredths,
                },
                pair @ (ContrastPair::AccentOnBackground
                | ContrastPair::BorderOnBackground
                | ContrastPair::SearchMatchOnBackground) => ThemeWarning::ChromeBelowFloor {
                    key: match pair {
                        ContrastPair::AccentOnBackground => ThemeKey::AccentColor,
                        ContrastPair::BorderOnBackground => ThemeKey::BorderColor,
                        _ => ThemeKey::SearchMatchColor,
                    },
                    hundredths: row.hundredths,
                    floor_hundredths: row.minimum.unwrap_or(0),
                },
                other => panic!("{other:?} has no bar"),
            })
            .collect();
        let admitted: Vec<ThemeWarning> = admission_warnings(scheme, scheme.appearance)
            .into_iter()
            .filter(|w| {
                !matches!(
                    w,
                    ThemeWarning::AnsiMatchesBackground { .. }
                        | ThemeWarning::SearchMatchLikeSelection
                )
            })
            .collect();
        assert_eq!(admitted, from_rows, "{}", scheme.name);
        warned += admitted.len();
        for row in &report {
            if let Some(minimum) = row.minimum {
                assert_eq!(row.below_minimum(), row.hundredths < minimum, "{row:?}");
            }
        }
        let has = |pair| report.iter().any(|row| row.pair == pair);
        assert_eq!(
            has(ContrastPair::SelectionTextOnSelection),
            scheme.selection_fg.is_some()
        );
        assert_eq!(
            has(ContrastPair::AccentOnBackground),
            scheme.chrome.accent.is_some()
        );
        assert_eq!(
            has(ContrastPair::BorderOnBackground),
            scheme.chrome.border.is_some()
        );
        assert!((0u8..16).all(|slot| has(ContrastPair::AnsiOnBackground(slot))));
    }
    assert!(
        warned > 0,
        "the sweep must include themes that earn a warning"
    );
}

/// A scheme with no chrome omits the `chrome` key when serialized,
/// and one with chrome round-trips.
#[cfg(feature = "serde")]
#[test]
fn chrome_serializes_only_when_authored() {
    let plain = ColorScheme::default();
    let text = aterm_json::to_string(&plain).expect("serializes");
    assert!(!text.contains("chrome"), "{text}");
    let back: ColorScheme = aterm_json::from_str(&text).expect("reads back");
    assert_eq!(back, plain);
    let mut authored = plain;
    authored.chrome.accent = Some(Rgb::new(0x7a, 0xa2, 0xf7));
    let text = aterm_json::to_string(&authored).expect("serializes");
    assert!(text.contains("\"chrome\""), "{text}");
    let back: ColorScheme = aterm_json::from_str(&text).expect("reads back");
    assert_eq!(back, authored);
}
