// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// THE KEPT PALETTE IS THE ONE A SOLVE GIVES: whatever a thread asked before, [`roles`]
/// answers what [`solve_roles`] answers now, across each thing a solve reads (each theme
/// colour, the ANSI hues, the authored accent and border, the preferences, the OS and control
/// accents). Each
/// asked palette differs from the first in one of them and solves apart from it, and is read
/// straight after it and before it, so a key blind to that reading answers for the other. Asked
/// again unchanged it solves nothing, and it keeps no more than [`ROLES_KEPT`].
/// NEGATIVE CONTROL (2026-11-05): with the key blind to the theme (a default theme in its place)
/// this fired "the palette kept under another reading" on the selection colour's case; blind to
/// the seeds, on the ANSI hues' case; blind to the authored border alone, on that border's; blind
/// to the preferences, on high contrast's; blind to the control accent, on that accent's. With a
/// kept palette never found it fired "asked again it solves nothing" (left 1, right 0), and with
/// every solve kept "it keeps what it may, no more" (left 11, right 6).
#[test]
fn a_kept_palette_is_the_one_a_solve_gives() {
    use aterm_messages::ink::AnsiHues;
    use aterm_messages::tokens::AuthoredChrome;

    use crate::chrome_band::ChromePalette;
    // Palette, preferences, the OS's accent and the control's.
    type Asked = (
        ChromePalette,
        AppearancePreferences,
        Option<SystemAccent>,
        Option<SystemAccent>,
    );
    let grey = Theme {
        cursor: 0x0080_8080,
        ..Theme::default()
    };
    let plain = ChromePalette::from(grey);
    let accent = SystemAccent {
        on_dark: [0xFF, 0x9F, 0x0A],
        on_light: [0x95, 0x3D, 0x96],
    };
    let hues = AnsiHues {
        red: [0xF0, 0x30, 0x30],
        green: [0x30, 0xF0, 0x30],
        yellow: [0xF0, 0xF0, 0x30],
        blue: [0x30, 0x70, 0xF0],
        cyan: [0x30, 0xF0, 0xF0],
    };
    let base = AppearancePreferences::default();
    let palette_of = |palette: ChromePalette| -> Asked { (palette, base, None, None) };
    let themed = |theme: Theme| palette_of(ChromePalette { theme, ..plain });
    let asked: Vec<Asked> = vec![
        palette_of(plain),
        themed(Theme {
            fg: 0x00FF_FFFF,
            ..grey
        }),
        themed(Theme {
            bg: 0x0030_1018,
            ..grey
        }),
        themed(Theme {
            cursor: 0x00FF_3060,
            ..grey
        }),
        themed(Theme {
            selection: 0x00E0_40A0,
            ..grey
        }),
        palette_of(ChromePalette {
            ansi: Some(hues),
            ..plain
        }),
        palette_of(ChromePalette {
            authored: AuthoredChrome {
                accent: Some([0x10, 0xC0, 0x90]),
                ..AuthoredChrome::NONE
            },
            ..plain
        }),
        palette_of(ChromePalette {
            authored: AuthoredChrome {
                border: Some([0xC0, 0x30, 0x90]),
                ..AuthoredChrome::NONE
            },
            ..plain
        }),
        (
            plain,
            AppearancePreferences {
                high_contrast: true,
                ..base
            },
            None,
            None,
        ),
        (plain, base, Some(accent), None),
        (plain, base, None, Some(accent)),
    ];
    assert!(
        asked.len() > ROLES_KEPT,
        "the asking overflows what is kept"
    );
    let set = |(_, _, system, control): &Asked| {
        let _ = install_system_accent(*system);
        let _ = install_control_accent(*control);
    };
    let answers: Vec<NativeRoles> = asked
        .iter()
        .map(|asked| {
            set(asked);
            solve_roles(asked.0, asked.1.normalized(), control_accent())
        })
        .collect();
    for (at, answer) in answers.iter().enumerate().skip(1) {
        assert_ne!(*answer, answers[0], "it moves the palette: {:?}", asked[at]);
    }
    for at in 0..asked.len() {
        for each in [0, at, at, 0] {
            set(&asked[each]);
            assert_eq!(
                roles(asked[each].0, asked[each].1),
                answers[each],
                "the palette kept under another reading: {:?}",
                asked[each]
            );
        }
    }
    KEPT_ROLES.with_borrow(|kept| {
        assert_eq!(
            kept.solved.len(),
            ROLES_KEPT,
            "it keeps what it may, no more"
        );
    });
    set(&asked[0]);
    let first = roles(plain, base);
    let _ = crate::work_counts::take_roles_solved();
    assert_eq!(roles(plain, base), first);
    assert_eq!(
        crate::work_counts::take_roles_solved(),
        0,
        "asked again it solves nothing"
    );
    let _ = take_chrome_inputs_moved();
}

#[test]
fn the_accessibility_seam_turns_on_what_it_names_and_nothing_else() {
    assert_eq!(parse_dev_accessibility(""), DevAccessibility::default());
    assert_eq!(
        parse_dev_accessibility("high-contrast, reduce-motion"),
        DevAccessibility {
            high_contrast: true,
            reduce_motion: true,
            ..DevAccessibility::default()
        }
    );
    assert_eq!(
        parse_dev_accessibility("reduce-transparency,differentiate-without-color,dark"),
        DevAccessibility {
            reduced_transparency: true,
            differentiate_without_color: true,
            ..DevAccessibility::default()
        },
        "an unknown word turns nothing on"
    );
}

#[test]
fn the_os_accent_leads_the_chrome_where_none_is_authored() {
    use aterm_messages::tokens::AccentSource;
    let _ = take_chrome_inputs_moved();
    let theme = Theme::default();
    let cursor_accent = crate::chrome_band::ChromePalette::from(theme)
        .tokens()
        .accent;
    let windows = SystemAccent {
        on_dark: [0x4c, 0xc2, 0xff],
        on_light: [0x00, 0x67, 0xc0],
    };
    assert!(
        install_system_accent(Some(windows)),
        "none → some is a move"
    );
    assert!(take_chrome_inputs_moved(), "and the windows are told");
    let tokens = crate::chrome_band::ChromePalette::from(theme).tokens();
    assert_eq!(tokens.accent_source, AccentSource::System);
    let bg = [
        (theme.bg >> 16) as u8,
        (theme.bg >> 8) as u8,
        theme.bg as u8,
    ];
    assert_eq!(tokens.accent, windows.for_surface(bg));
    let roles = roles(theme, AppearancePreferences::default());
    let hue = |c: [u8; 3]| rgb_to_oklch(c).h;
    assert!(
        (hue(roles.accent) - hue(windows.for_surface(bg))).abs() < 0.2,
        "the Settings accent keeps the OS accent's hue: {:?}",
        roles.accent
    );
    assert!(!install_system_accent(Some(windows)), "no move, no repaint");
    assert!(install_system_accent(None));
    assert_eq!(
        crate::chrome_band::ChromePalette::from(theme)
            .tokens()
            .accent,
        cursor_accent
    );
    let _ = take_chrome_inputs_moved();
}

/// The installed accent is stored packed and reads back as the pair it was, each shade on
/// its own surface; no pair packs to the "no accent" value.
#[test]
fn a_packed_accent_reads_back_as_its_pair_and_never_as_none() {
    for accent in [
        SystemAccent {
            on_dark: [0x12, 0x34, 0x56],
            on_light: [0xAB, 0xCD, 0xEF],
        },
        SystemAccent {
            on_dark: [0xFF; 3],
            on_light: [0xFF; 3],
        },
        SystemAccent {
            on_dark: [0; 3],
            on_light: [0; 3],
        },
    ] {
        assert_eq!(SystemAccent::unpack(accent.pack()), accent);
        assert_ne!(accent.pack(), NO_SYSTEM_ACCENT, "{accent:?}");
    }
}

/// THE CONTROL ACCENT FOLLOWS THE SYSTEM'S (init-on-by-default design §4.4, D1): the
/// `controlAccentColor` the platform read is what a native control wears, its dark shade on
/// a dark ground and its light one on a light ground, and another reading moves it; with
/// none read a Mac wears system blue (`#0A84FF` / `#007AFF`), elsewhere the chrome's accent.
/// NEGATIVE CONTROL: the chrome's own accent never moves with it.
#[test]
fn the_control_accent_follows_the_systems() {
    let light = aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| Theme::from(&aterm_types::scheme::builtin(name).expect("a builtin")))
        .find(|theme| !surface_is_dark(packed_rgb(theme.bg)))
        .expect("a light builtin theme");
    let dark = Theme::default();
    let _ = install_control_accent(None);
    let _ = take_chrome_inputs_moved();
    let worn = |theme: Theme| roles(theme, AppearancePreferences::default()).control_accent;
    let chrome = |theme: Theme| roles(theme, AppearancePreferences::default()).accent;
    let (chrome_dark, chrome_light) = (chrome(dark), chrome(light));
    // None read: a Mac's system blue, else the chrome's accent.
    if cfg!(target_os = "macos") {
        assert_eq!(worn(dark), [0x0A, 0x84, 0xFF]);
        assert_eq!(worn(light), [0x00, 0x7A, 0xFF]);
    } else {
        assert_eq!((worn(dark), worn(light)), (chrome_dark, chrome_light));
    }
    for reading in [
        SystemAccent {
            on_dark: [0xA5, 0x50, 0xA7],
            on_light: [0x95, 0x3D, 0x96],
        },
        SystemAccent {
            on_dark: [0xFF, 0x9F, 0x0A],
            on_light: [0xF7, 0x82, 0x1B],
        },
    ] {
        assert!(install_control_accent(Some(reading)), "a new reading moves");
        assert!(take_chrome_inputs_moved(), "and the windows are told");
        assert_eq!(
            worn(dark),
            reading.on_dark,
            "the dark shade on a dark ground"
        );
        assert_eq!(
            worn(light),
            reading.on_light,
            "the light shade on a light ground"
        );
        assert_eq!(
            (chrome(dark), chrome(light)),
            (chrome_dark, chrome_light),
            "the chrome keeps its theme's accent"
        );
        assert!(
            !install_control_accent(Some(reading)),
            "no move, no repaint"
        );
    }
    assert!(install_control_accent(None));
    if cfg!(target_os = "macos") {
        assert_eq!(worn(dark), MACOS_SYSTEM_BLUE.on_dark);
    }
    let _ = take_chrome_inputs_moved();
}

#[test]
fn a_consumed_preference_edge_still_reaches_the_resample() {
    let previous = current_preferences();
    let _ = take_chrome_inputs_moved();

    let moved = AppearancePreferences {
        high_contrast: !previous.high_contrast,
        reduced_transparency: !previous.reduced_transparency,
        text_scale: 1.25,
        differentiate_without_color: previous.differentiate_without_color,
    };
    assert!(install_preferences(moved), "off → on is a change");
    assert!(
        !install_preferences(moved),
        "the edge is gone: a second install reports no movement"
    );
    assert!(
        take_chrome_inputs_moved(),
        "the preference move was swallowed by the non-resample reader"
    );
    assert!(
        !take_chrome_inputs_moved(),
        "the latch is drained by its one consumer"
    );

    install_preferences(previous);
    let _ = take_chrome_inputs_moved();
}

#[test]
fn differentiate_without_color_is_a_paint_input() {
    let previous = current_preferences();
    let _ = take_chrome_inputs_moved();
    let on = AppearancePreferences {
        differentiate_without_color: !previous.differentiate_without_color,
        ..previous
    };
    assert!(install_preferences(on), "the setting moved the chrome");
    assert_eq!(
        current_preferences().differentiate_without_color,
        on.differentiate_without_color
    );
    assert!(take_chrome_inputs_moved());
    assert!(install_preferences(previous), "and back");
    assert_eq!(current_preferences(), previous);
    let _ = take_chrome_inputs_moved();
}

const THEMES: [Theme; 6] = [
    Theme {
        fg: 0x00D0_D0D0,
        bg: 0x0011_1318,
        cursor: 0x0050_FA7B,
        selection: 0x0033_415E,
    },
    Theme {
        fg: 0x0024_293A,
        bg: 0x00FA_F9F5,
        cursor: 0x004C_6FFF,
        selection: 0x00D8_E2FF,
    },
    Theme {
        fg: 0x00FF_FFFF,
        bg: 0x0000_0000,
        cursor: 0x00FF_FFFF,
        selection: 0x0040_4040,
    },
    Theme {
        fg: 0x0000_0000,
        bg: 0x00FF_FFFF,
        cursor: 0x0000_0000,
        selection: 0x00C8_C8C8,
    },
    Theme {
        fg: 0x00FF_E8F3,
        bg: 0x0028_0F22,
        cursor: 0x00FF_4FA3,
        selection: 0x006B_244F,
    },
    Theme {
        fg: 0x00E4_F7FF,
        bg: 0x0007_2634,
        cursor: 0x0000_F5FF,
        selection: 0x0015_526B,
    },
];

fn bundled_themes() -> Vec<(&'static str, Theme)> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            (
                name,
                Theme::from(&aterm_types::scheme::builtin(name).expect("a listed builtin exists")),
            )
        })
        .collect()
}

fn assert_role_contract(what: &str, r: NativeRoles) {
    assert!(
        contrast_ratio(r.elevated, r.surface) >= CARD_CONTRAST_MIN,
        "{what}: opaque card against the page"
    );
    for (on, backdrop) in [("card", r.elevated), ("page", r.surface)] {
        assert!(
            contrast_ratio(r.text_primary, backdrop) >= 4.5,
            "{what}: primary text on the {on}"
        );
        assert!(
            contrast_ratio(r.text_secondary, backdrop) >= 4.5,
            "{what}: secondary text on the {on}"
        );
        assert!(
            contrast_ratio(r.text_tertiary, backdrop) >= 3.0,
            "{what}: tertiary text on the {on}"
        );
        assert!(
            contrast_ratio(r.danger, backdrop) >= 4.5,
            "{what}: danger on the {on}"
        );
        assert!(
            contrast_ratio(r.success, backdrop) >= 4.5,
            "{what}: success on the {on}"
        );
    }
    assert!(
        contrast_ratio(r.accent, r.surface) >= ACCENT_INK_FLOOR,
        "{what}: accent as a rule on the page"
    );
    for (state, mix) in [
        ("selected", ACCENT_WASH_SELECTED),
        ("pressed", ACCENT_WASH_PRESSED),
    ] {
        let wash = mix_srgb(r.elevated, r.accent, mix);
        let ratio = contrast_ratio(r.accent, wash);
        assert!(
            ratio >= ACCENT_INK_FLOOR,
            "{what}: accent ink on the {state} row's own wash is {ratio:.2}:1"
        );
    }
    assert!(
        contrast_ratio(r.on_accent, r.accent) >= 4.5,
        "{what}: ink on an accent fill"
    );
    for (ground, rgb) in [
        ("surface", r.surface),
        ("elevated", r.elevated),
        (
            "pressed wash",
            mix_srgb(r.elevated, r.accent, ACCENT_WASH_PRESSED),
        ),
    ] {
        let ratio = contrast_ratio(r.accent_ink, rgb);
        assert!(
            ratio >= 4.5,
            "{what}: accent as small text on {ground} is {ratio:.2}:1"
        );
    }
    for (on, backdrop) in [("card", r.elevated), ("page", r.surface)] {
        let ratio = contrast_ratio(r.control_edge, backdrop);
        assert!(
            ratio >= 3.0,
            "{what}: a control's edge on the {on} is {ratio:.2}:1"
        );
    }
}

#[test]
fn cards_preserve_the_preferred_direction_when_both_grounds_remain_readable() {
    for (surface, preferred_rise, expected_rise) in [
        ([32; 3], true, true),
        ([224; 3], true, true),
        ([224; 3], false, false),
        // A brighter card loses white text here; black cannot read the page.
        ([111; 3], true, false),
        // A darker card loses black text here; white cannot read the page.
        ([124; 3], false, true),
        // There is not enough room before white for the preferred rise.
        ([250; 3], true, false),
    ] {
        let card = card_surface(surface, surface, preferred_rise, 4.5);
        let grounds = [surface, card];
        assert!(contrast_ratio(card, surface) >= CARD_CONTRAST_MIN);
        assert_eq!(
            relative_luminance(card) > relative_luminance(surface),
            expected_rise,
            "page {surface:?}, preferred rise {preferred_rise}"
        );
        for seed in [[0; 3], [128; 3], [255; 3]] {
            let ink = ensure_contrast_on(seed, &grounds, 4.5);
            assert!(least_contrast(ink, &grounds) >= 4.5);
        }
    }
}

#[test]
fn roles_meet_their_contrast_contract_on_diverse_themes() {
    for theme in THEMES {
        assert_role_contract(&format!("{theme:?}"), default_roles(theme));
    }
    for (name, theme) in bundled_themes() {
        assert_role_contract(name, default_roles(theme));
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
        let hostile = packed_rgb(theme.bg);
        for authored in [
            aterm_messages::tokens::AuthoredChrome::NONE,
            aterm_messages::tokens::AuthoredChrome {
                accent: Some(hostile),
                border: Some(hostile),
                ..aterm_messages::tokens::AuthoredChrome::NONE
            },
        ] {
            let palette = crate::chrome_band::ChromePalette {
                theme,
                ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                authored,
            };
            assert_role_contract(name, default_roles(palette));
        }
    }
}

#[test]
fn grouped_surfaces_keep_shared_text_readable_on_a_low_contrast_theme() {
    let theme = Theme {
        bg: 0x0076_7676,
        fg: 0x0076_7676,
        ..Theme::default()
    };
    for high_contrast in [false, true] {
        let r = roles(
            theme,
            AppearancePreferences {
                high_contrast,
                ..AppearancePreferences::default()
            },
        );
        assert!(
            !surface_is_dark(r.surface),
            "exercise light grouped grounds"
        );
        let grouped = r.grouped();
        assert!(contrast_ratio(grouped.page, grouped.card) >= CARD_CONTRAST_MIN);
        for (ink, normal_floor) in [
            (r.text_primary, 4.5),
            (r.text_secondary, 4.5),
            (r.text_tertiary, 3.0),
        ] {
            let original = least_contrast(ink, &[r.surface, r.elevated]);
            for ground in [grouped.page, grouped.card] {
                let contrast = contrast_ratio(ink, ground);
                assert!(
                    contrast >= original,
                    "{ink:?} on {ground:?}: {contrast} < original {original}, high contrast {high_contrast}"
                );
                if !high_contrast {
                    assert!(contrast >= normal_floor);
                }
            }
        }
    }
}

#[test]
fn accent_text_keeps_headroom_over_plain_and_selected_surfaces() {
    for theme in THEMES
        .into_iter()
        .chain(bundled_themes().into_iter().map(|(_, theme)| theme))
    {
        let r = default_roles(theme);
        let grounds = [
            r.surface,
            r.elevated,
            mix_srgb(r.elevated, r.accent, ACCENT_WASH_SELECTED),
            mix_srgb(r.elevated, r.accent, ACCENT_WASH_PRESSED),
        ];
        let target = 5.0_f32.min(best_endpoint_contrast(&grounds));
        assert!(
            least_contrast(r.accent_ink, &grounds) >= target,
            "accent text {:?} on {grounds:?} must retain {target}:1",
            r.accent_ink
        );
    }
}

#[test]
fn native_accent_is_the_token_accent() {
    use aterm_messages::tokens::{AccentSource, AuthoredChrome};
    let hue_gap = |a: [u8; 3], b: [u8; 3]| {
        let (x, y) = (rgb_to_oklch(a).h, rgb_to_oklch(b).h);
        let d = (x - y).rem_euclid(TAU);
        d.min(TAU - d).to_degrees()
    };
    let mut checked = 0;
    for (name, theme) in bundled_themes() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
        for authored in [
            AuthoredChrome::NONE,
            AuthoredChrome {
                accent: Some([0xff, 0x00, 0xaa]),
                ..AuthoredChrome::NONE
            },
        ] {
            let palette = crate::chrome_band::ChromePalette {
                theme,
                ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                authored,
            };
            let tokens = palette.tokens();
            if tokens.accent_source == AccentSource::Neutral
                || rgb_to_oklch(tokens.accent).c < 0.035
            {
                continue;
            }
            let accent = default_roles(palette).accent;
            let gap = hue_gap(accent, tokens.accent);
            assert!(
                gap < 12.0,
                "{name} {:?}: accent {accent:02x?} is {gap:.1} degrees off {:02x?}",
                tokens.accent_source,
                tokens.accent
            );
            checked += 1;
        }
    }
    assert!(checked > 10, "CONTROL: accents to compare ({checked})");
    let mono = Theme {
        fg: 0x00D0_D0D0,
        bg: 0x0010_1010,
        cursor: 0x00C0_C0C0,
        selection: 0x0040_4040,
    };
    let grey = aterm_messages::ink::AnsiHues {
        red: [0x60; 3],
        green: [0x70; 3],
        yellow: [0x80; 3],
        blue: [0x90; 3],
        cyan: [0xa0; 3],
    };
    let palette = crate::chrome_band::ChromePalette {
        theme: mono,
        ansi: Some(grey),
        authored: AuthoredChrome::NONE,
    };
    assert_eq!(palette.tokens().accent_source, AccentSource::Neutral);
    assert_eq!(default_roles(palette), default_roles(mono));
}

#[test]
fn resolve_chrome_palette_forces_the_whole_band_or_follows_the_system() {
    use crate::app_config::WindowTheme;
    let dark_term = Theme {
        bg: 0x0000_0000,
        fg: 0x00ff_ffff,
        cursor: 0x0000_ff00,
        selection: 0x0033_3333,
    };
    let light_term = Theme {
        bg: 0x00ff_ffff,
        fg: 0x0000_0000,
        cursor: 0x0000_00ff,
        selection: 0x00cc_cccc,
    };

    assert_eq!(
        resolve_chrome_palette(WindowTheme::Auto, dark_term),
        dark_term
    );
    assert_eq!(
        resolve_chrome_palette(WindowTheme::Auto, light_term),
        light_term
    );

    let forced = resolve_chrome_palette(WindowTheme::Light, dark_term);
    assert_ne!(
        forced, dark_term,
        "forced light must re-author, not pass the dark band through"
    );
    assert!(
        !crate::tab_bar::theme_is_dark(forced.bg),
        "forced light must yield a LIGHT chrome bg (matches the light caption)"
    );
    assert_eq!(forced, forced_chrome_theme(dark_term, false));

    let forced_dark = resolve_chrome_palette(WindowTheme::Dark, light_term);
    assert!(crate::tab_bar::theme_is_dark(forced_dark.bg));
    assert_eq!(forced_dark, forced_chrome_theme(light_term, true));

    assert_eq!(
        resolve_chrome_palette(WindowTheme::Dark, dark_term),
        dark_term
    );
    assert_eq!(
        resolve_chrome_palette(WindowTheme::Light, light_term),
        light_term
    );
}

#[test]
fn forced_chrome_themes_meet_the_roles_contract_for_any_carried_accent() {
    let donors = THEMES
        .into_iter()
        .map(|theme| ("hand-written", theme))
        .chain(bundled_themes());
    for (name, terminal) in donors {
        for dark in [false, true] {
            let forced = forced_chrome_theme(terminal, dark);
            assert_eq!(surface_is_dark(packed_rgb(forced.bg)), dark);
            assert!(
                contrast_ratio(packed_rgb(forced.fg), packed_rgb(forced.bg)) >= 7.0,
                "authored chrome text is comfortably readable on its own surface"
            );
            assert!(
                contrast_ratio(packed_rgb(forced.cursor), packed_rgb(forced.bg))
                    >= FORCED_CHROME_ACCENT_FLOOR - 0.01
            );
            assert_role_contract(
                &format!("{name} carried onto the forced dark={dark} chrome"),
                default_roles(forced),
            );
        }
    }
    let light = forced_chrome_theme(THEMES[1], false);
    assert_eq!(light.bg, FORCED_LIGHT_CHROME_BG);
    assert_eq!(light.fg, FORCED_LIGHT_CHROME_FG);
    assert_eq!(light.selection, THEMES[1].selection);
}

#[test]
fn high_contrast_strengthens_secondary_and_separators() {
    for theme in THEMES {
        let normal = default_roles(theme);
        let high = roles(
            theme,
            AppearancePreferences {
                high_contrast: true,
                text_scale: 1.0,
                ..AppearancePreferences::default()
            },
        );
        for backdrop in [high.surface, high.elevated] {
            assert!(contrast_ratio(high.text_primary, backdrop) >= 7.0);
            assert!(contrast_ratio(high.text_secondary, backdrop) >= 7.0);
            assert!(contrast_ratio(high.text_tertiary, backdrop) >= 4.5);
        }
        assert!(contrast_ratio(high.separator, high.surface) >= 3.0);
        assert!(
            contrast_ratio(high.separator, high.surface)
                >= contrast_ratio(normal.separator, normal.surface)
        );
    }
}

#[test]
fn an_injected_text_scale_survives_a_resample_until_cleared() {
    super::inject_text_scale(Some(2.0));
    super::install_preferences(super::AppearancePreferences::default());
    assert_eq!(super::text_scale(), 2.0);
    assert_eq!(
        super::text_scale_status_line(),
        "text_scale=2 source=injected"
    );
    super::inject_text_scale(None);
    super::install_preferences(super::AppearancePreferences::default());
    assert_eq!(super::text_scale(), 1.0);
    assert_eq!(super::text_scale_status_line(), "text_scale=1 source=os");

    assert_eq!(super::text_scale_request("", false), Ok(None));
    assert_eq!(super::text_scale_request("1.5", true), Ok(Some(Some(1.5))));
    assert_eq!(super::text_scale_request("os", true), Ok(Some(None)));
    for bad in ["3", "0.5", "big", "1.5 2"] {
        assert!(super::text_scale_request(bad, true).is_err(), "{bad}");
    }
    assert!(
        super::text_scale_request("1.5", false).is_err(),
        "no injection without the seam"
    );
}

#[test]
fn text_scale_is_sane_and_bounded() {
    let norm = |text_scale: f32| {
        AppearancePreferences {
            text_scale,
            ..AppearancePreferences::default()
        }
        .normalized()
        .text_scale
    };
    assert_eq!(
        AppearancePreferences::default().normalized().text_scale,
        1.0
    );
    for non_positive_or_non_finite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.5] {
        assert_eq!(norm(non_positive_or_non_finite), 1.0);
    }
    assert_eq!(norm(0.5), 0.85);
    assert_eq!(norm(0.85), 0.85);
    assert_eq!(norm(1.25), 1.25);
    assert_eq!(norm(2.0), 2.0);
    assert_eq!(norm(9.0), 2.0);
}
