// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn table_is_sorted() {
    for window in X11_COLORS.windows(2) {
        assert!(
            window[0].0 < window[1].0,
            "X11 color table not sorted: {:?} >= {:?}",
            window[0].0,
            window[1].0
        );
    }
}

#[test]
fn lookup_case_insensitive() {
    assert_eq!(lookup("Red"), Some(Rgb::new(255, 0, 0)));
    assert_eq!(lookup("RED"), Some(Rgb::new(255, 0, 0)));
    assert_eq!(lookup("DarkSlateGray"), Some(Rgb::new(47, 79, 79)));
    assert_eq!(lookup("DARKSLATEGRAY"), Some(Rgb::new(47, 79, 79)));
    assert_eq!(lookup("darkslategray"), Some(Rgb::new(47, 79, 79)));
}

#[test]
fn lookup_grey_variants() {
    assert_eq!(lookup("gray"), lookup("grey"));
    assert_eq!(lookup("darkgray"), lookup("darkgrey"));
    assert_eq!(lookup("lightgray"), lookup("lightgrey"));
    assert_eq!(lookup("slategray"), lookup("slategrey"));
}

#[test]
fn lookup_unknown_returns_none() {
    assert_eq!(lookup("notacolor"), None);
    assert_eq!(lookup(""), None);
    assert_eq!(lookup("redd"), None);
    assert_eq!(lookup("r\u{00e9}d"), None);
}
