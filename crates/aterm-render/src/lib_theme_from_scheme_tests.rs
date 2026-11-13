// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Theme;
use aterm_types::scheme::{self, ColorScheme};

#[test]
fn the_default_scheme_is_the_default_theme() {
    assert_eq!(Theme::from(&ColorScheme::default()), Theme::default());
}

#[test]
fn every_builtin_converts_slot_for_slot() {
    for name in scheme::builtin_names() {
        let s = scheme::builtin(name).expect("a listed scheme");
        let parts = s.to_theme_parts();
        let theme = Theme::from(parts);
        assert_eq!(
            (theme.fg, theme.bg, theme.cursor, theme.selection),
            (parts.fg, parts.bg, parts.cursor, parts.selection),
            "{name}"
        );
        assert_eq!(Theme::from(&s), theme, "{name}");
    }
}
