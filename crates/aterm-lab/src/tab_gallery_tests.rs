// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_chrome_reply_names_each_chip() {
    let got = chips(
        "toolbar-tabs count=2\n\
         tab-frame index=0 surface=terminal selected=false x=100 y=10 w=200 h=60 icon=-\n\
         tab-frame index=1 surface=app selected=true x=300 y=10 w=200 h=60 icon=340,26,28,28\n",
    );
    assert_eq!(got.len(), 2);
    assert!(!got[0].app && got[0].icon.is_none());
    assert!(got[1].app && got[1].selected);
    assert_eq!(got[1].icon, Some(px_box(340, 26, 28, 28)));
}

#[test]
fn a_strip_with_no_tile_fails_the_layout_check() {
    let chip = |app, icon: bool| Chip {
        app,
        selected: false,
        frame: px_box(0, 0, 10, 10),
        icon: icon.then(|| px_box(1, 1, 5, 5)),
    };
    let good: Vec<Chip> = WIDE_APPS.iter().map(|app| chip(*app, *app)).collect();
    assert!(wide_layout_holds(&good).is_ok());
    let bare: Vec<Chip> = WIDE_APPS.iter().map(|app| chip(*app, false)).collect();
    assert!(wide_layout_holds(&bare).is_err());
    assert!(wide_layout_holds(&[]).is_err());
}

#[test]
fn a_tile_is_judged_by_its_own_state() {
    let reading = TileReading {
        ground: [0x20, 0x20, 0x20],
        fill: [0x3c, 0x3c, 0x3c],
        glyph: [0xe0, 0xe0, 0xe0],
    };
    assert!(judge(&reading, false).iter().all(|(_, pass)| *pass));
    assert!(
        !judge(&reading, true)[0].1,
        "a selected tile is held to 3:1"
    );
}

#[test]
fn every_theme_name_makes_a_file_name() {
    assert_eq!(slug("Solarized Dark"), "solarized-dark");
    assert!(ALL.iter().all(|t| !slug(t).contains(' ')));
}

#[test]
fn a_custom_theme_parses_as_the_theme_it_names() {
    for custom in &CUSTOM {
        let parsed = aterm_types::scheme::parse_theme_str(&custom_theme_text(custom));
        assert!(parsed.is_ok(), "{}", custom.0);
    }
}
