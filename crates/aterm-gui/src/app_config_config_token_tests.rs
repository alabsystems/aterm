// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Config, RightClickGesture, TabBandHeight, TabMenuChord, WindowColorspace, WindowTheme,
};

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn enum_keys_parse_trimmed_any_case_with_aliases_and_fall_back() {
    let theme = |t: &str| cfg(t).window_theme_or_default();
    assert_eq!(theme("window_theme = \" Dark \""), WindowTheme::Dark);
    assert_eq!(theme("window_theme = \"LIGHT\""), WindowTheme::Light);
    assert_eq!(theme("window_theme = \"midnight\""), WindowTheme::Auto);
    assert_eq!(theme(""), WindowTheme::Auto);

    let band = |t: &str| cfg(t).tab_band_height_or_default();
    let standard = "tab_band_height = \"  STANDARD \"";
    assert_eq!(band(standard), TabBandHeight::Standard);
    assert_eq!(
        band("tab_band_height = \"compact\""),
        TabBandHeight::Compact
    );
    let unknown = "tab_band_height = \"huge\"";
    assert_eq!(band(unknown), TabBandHeight::PLATFORM_DEFAULT);

    let click = |t: &str| cfg(t).right_click_or_default();
    let copy_paste = "right_click = \" Copy-Paste \"";
    assert_eq!(click(copy_paste), RightClickGesture::CopyPaste);
    assert_eq!(
        click("right_click = \"menu\""),
        RightClickGesture::PLATFORM_DEFAULT
    );
    assert_eq!(click(""), RightClickGesture::PLATFORM_DEFAULT);

    let space = |t: &str| cfg(t).window_colorspace_or_default();
    let p3 = "window_colorspace = \" DisplayP3 \"";
    assert_eq!(space(p3), WindowColorspace::DisplayP3);
    assert_eq!(
        space("window_colorspace = \"P3\""),
        WindowColorspace::DisplayP3
    );
    let rec2020 = "window_colorspace = \"rec2020\"";
    assert_eq!(space(rec2020), WindowColorspace::Srgb);
    assert_eq!(space(""), WindowColorspace::Srgb);

    for (source, chord, menu_key, shift_f10) in [
        ("", TabMenuChord::MenuKey, true, false),
        (
            "tab_menu_chord = \"nope\"",
            TabMenuChord::MenuKey,
            true,
            false,
        ),
        (
            "tab_menu_chord = \" Menu-Key \"",
            TabMenuChord::MenuKey,
            true,
            false,
        ),
        ("tab_menu_chord = \"BOTH\"", TabMenuChord::On, true, true),
        ("tab_menu_chord = \"off\"", TabMenuChord::Off, false, false),
    ] {
        let got = cfg(source).tab_menu_chord_or_default();
        assert_eq!(got, chord, "{source}");
        assert_eq!(
            (got.claims_menu_key(), got.claims_shift_f10()),
            (menu_key, shift_f10),
            "{source}"
        );
    }
}
