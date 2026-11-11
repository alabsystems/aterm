// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named terminal color schemes and theme parsing.

use crate::{ColorPalette, Rgb};

pub use crate::scheme_typography::{
    MAX_SKIPPED_THEME_KEY_BYTES, MAX_SKIPPED_THEME_KEYS, MAX_THEME_FEATURES, MAX_THEME_FONT_LIST,
    MAX_THEME_FONT_NAME_CHARS, THEME_SCHEMA_VERSION, ThemeTypography,
};

/// Maximum bytes admitted from one user theme definition.
pub const MAX_USER_THEME_FILE_BYTES: usize = 256 * 1024;

/// Whether a scheme is meant for a dark or light background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Appearance {
    /// A scheme intended for a dark background (the default).
    #[default]
    Dark,
    /// A scheme intended for a light background.
    Light,
}

impl Appearance {
    /// The DEC color-scheme DSR parameter for this appearance (`1` = dark, `2` = light), as reported via DSR `CSI ? 996 n` → `CSI ? 997 ; Ps n` and the DEC mode 2031 push.
    #[inline]
    #[must_use]
    pub const fn dsr_code(self) -> u8 {
        match self {
            Appearance::Dark => 1,
            Appearance::Light => 2,
        }
    }
}

/// The renderer chrome of a scheme as packed `0x00RRGGBB` values — the exact shape `aterm_render::Theme` wants, but as plain `u32`s so this crate need not depend on the renderer (no crate cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeParts {
    /// Default foreground / window clear foreground.
    pub fg: u32,
    /// Default background / window clear color (also the interior-padding fill).
    pub bg: u32,
    /// Cursor color.
    pub cursor: u32,
    /// Selection highlight background.
    pub selection: u32,
}

/// A complete named color scheme: chrome + the 16 ANSI slots.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorScheme {
    /// Display name (e.g. `"Dracula"`); `""` for an anonymous inline scheme.
    pub name: String,
    /// Dark/light hint.
    pub appearance: Appearance,
    /// Default foreground.
    pub foreground: Rgb,
    /// Default background (and window clear / interior padding fill).
    pub background: Rgb,
    /// Cursor color; `None` follows the foreground (matches the engine's fallback).
    pub cursor: Option<Rgb>,
    /// Selection highlight bg; `None` derives one from this scheme's own background and foreground ([`derived_selection`]), so a light theme that leaves it out gets a light band and a dark one a dark band.
    pub selection: Option<Rgb>,
    /// Text colour inside the selection; `None` keeps each cell's own colour (the renderer floors it against the selection).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_fg: Option<Rgb>,
    /// The 16 ANSI slots: `0..8)` normal (black, red, green, yellow, blue, magenta, cyan, white), `8..16)` their bright variants.
    pub ansi: [Rgb; 16],
    /// The colours of aterm's own chrome the theme authors (`accent_color`, `active_tab_color`, `border_color`, `search_match_color`); empty for every built-in.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "ChromeColors::is_empty")
    )]
    pub chrome: ChromeColors,
    /// Whether slots 16-255 derive from this scheme's own colours ([`crate::scheme_palette256::generated_extended_palette`]) instead of keeping xterm's cube and grey ramp.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "core::ops::Not::not")
    )]
    pub palette_generate: bool,
}

/// The chrome colours a theme may author: each is optional and each is also an `aterm.toml` key spelled the same way, which beats the theme's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChromeColors {
    /// `accent_color`: the active-tab rule, Settings' selected rows, the find highlight's hue.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub accent: Option<Rgb>,
    /// `active_tab_color`: the selected tab's background.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_tab: Option<Rgb>,
    /// `border_color`: split-pane dividers and the chrome bands' seams.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub border: Option<Rgb>,
    /// `search_match_color`: find's highlight on every match but the current.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub search_match: Option<Rgb>,
}

impl ChromeColors {
    /// Whether the theme authors no chrome colour.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The slot a chrome key sets; `None` for a key that is not a chrome key.
    #[inline]
    #[must_use]
    pub const fn slot(&self, key: ThemeKey) -> Option<Option<Rgb>> {
        match key {
            ThemeKey::AccentColor => Some(self.accent),
            ThemeKey::ActiveTabColor => Some(self.active_tab),
            ThemeKey::BorderColor => Some(self.border),
            ThemeKey::SearchMatchColor => Some(self.search_match),
            _ => None,
        }
    }

    /// The slot a chrome key sets, to write; `None` for a key that is not a chrome key.
    #[inline]
    pub const fn slot_mut(&mut self, key: ThemeKey) -> Option<&mut Option<Rgb>> {
        match key {
            ThemeKey::AccentColor => Some(&mut self.accent),
            ThemeKey::ActiveTabColor => Some(&mut self.active_tab),
            ThemeKey::BorderColor => Some(&mut self.border),
            ThemeKey::SearchMatchColor => Some(&mut self.search_match),
            _ => None,
        }
    }
}

/// Default's selection colour (`#33415E`).
const SELECTION_DEFAULT: Rgb = Rgb::new(0x33, 0x41, 0x5E);

impl Default for ColorScheme {
    /// Default color scheme.
    fn default() -> Self {
        let mut ansi = [Rgb::new(0, 0, 0); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            *slot = ColorPalette::default_color((i & 0xF) as u8);
        }
        Self {
            name: "Default".to_string(),
            appearance: Appearance::Dark,
            foreground: Rgb::new(0xD0, 0xD0, 0xD0),
            background: Rgb::new(0x11, 0x13, 0x18),
            cursor: Some(Rgb::new(0x50, 0xFA, 0x7B)),
            selection: Some(SELECTION_DEFAULT),
            selection_fg: None,
            ansi,
            chrome: ChromeColors::default(),
            palette_generate: false,
        }
    }
}

impl ColorScheme {
    /// Build the engine indexed palette from this scheme: each of the 16 ANSI slots is applied via [`ColorPalette::set`].
    #[must_use]
    pub fn to_color_palette(&self) -> ColorPalette {
        let mut p = ColorPalette::new();
        for (i, c) in self.ansi.iter().enumerate() {
            p.set((i & 0xF) as u8, *c);
        }
        if self.palette_generate {
            let generated = crate::scheme_palette256::generated_extended_palette(
                &self.ansi,
                self.foreground,
                self.background,
            );
            for (slot, c) in (16u8..=255).zip(generated.iter()) {
                p.set(slot, *c);
            }
        }
        p
    }

    /// Project the chrome to packed `0x00RRGGBB` [`ThemeParts`].
    #[inline]
    #[must_use]
    pub const fn to_theme_parts(&self) -> ThemeParts {
        const fn pack(c: Rgb) -> u32 {
            ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
        }
        let cursor = match self.cursor {
            Some(c) => c,
            None => self.foreground,
        };
        ThemeParts {
            fg: pack(self.foreground),
            bg: pack(self.background),
            cursor: pack(cursor),
            selection: pack(self.selection_band()),
        }
    }

    /// The selection band this scheme paints: its own `selection`, else one derived from its background and foreground ([`derived_selection`]).
    #[inline]
    #[must_use]
    pub const fn selection_band(&self) -> Rgb {
        match self.selection {
            Some(c) => c,
            None => derived_selection(self.foreground, self.background),
        }
    }
}

/// How far an unset selection band sits from the background toward the foreground, in percent: far enough to read as a band (about 1.5:1 on the background for a normal-contrast theme), near enough that text on it keeps most of its contrast.
pub const DERIVED_SELECTION_PERCENT: u16 = 22;

/// The selection band for a scheme that does not name one: `bg` moved [`DERIVED_SELECTION_PERCENT`] of the way toward `fg`, per sRGB channel, rounded.
#[inline]
#[must_use]
pub const fn derived_selection(fg: Rgb, bg: Rgb) -> Rgb {
    const fn mix(b: u8, f: u8) -> u8 {
        let toward = DERIVED_SELECTION_PERCENT;
        let away = 100 - toward;
        let blended = ((b as u16) * away + (f as u16) * toward + 50) / 100;
        (blended & 0xFF) as u8
    }
    Rgb::new(mix(bg.r, fg.r), mix(bg.g, fg.g), mix(bg.b, fg.b))
}

/// A compile-time built-in scheme.
struct Builtin {
    name: &'static str,
    appearance: Appearance,
    /// One line for listings and the picker.
    description: &'static str,
    /// The same family's built-in for the other appearance, if aterm ships it.
    pair: Option<&'static str>,
    /// Whether this is one of aterm's signature themes.
    signature: bool,
    /// The project a port's palette comes from; `None` for aterm's own.
    source: Option<BuiltinSource>,
    fg: Rgb,
    bg: Rgb,
    cursor: Rgb,
    selection: Rgb,
    ansi: [Rgb; 16],
}

impl Builtin {
    const fn identity(&self) -> BuiltinTheme {
        BuiltinTheme {
            name: self.name,
            appearance: self.appearance,
            description: self.description,
            pair: self.pair,
            signature: self.signature,
            source: self.source,
        }
    }

    fn to_scheme(&self) -> ColorScheme {
        ColorScheme {
            name: self.name.to_string(),
            appearance: self.appearance,
            foreground: self.fg,
            background: self.bg,
            cursor: Some(self.cursor),
            selection: Some(self.selection),
            selection_fg: None,
            ansi: self.ansi,
            chrome: ChromeColors::default(),
            palette_generate: self.signature,
        }
    }
}

const fn c(r: u8, g: u8, b: u8) -> Rgb {
    Rgb::new(r, g, b)
}

/// Bundled built-in color schemes.
static BUILTINS: &[Builtin] = &[
    Builtin {
        name: "Aterm Night",
        appearance: Appearance::Dark,
        description: "aterm's signature night: low-glare blue-black, cool slate ink and a sea-glass cyan cursor",
        pair: Some("Aterm Day"),
        signature: true,
        source: None,
        fg: c(0xd8, 0xe1, 0xec),
        bg: c(0x12, 0x17, 0x21),
        cursor: c(0x56, 0xd2, 0xdf),
        selection: c(0x2b, 0x3a, 0x57),
        ansi: [
            c(0x1f, 0x25, 0x32),
            c(0xe2, 0x7d, 0x7b),
            c(0x63, 0xb4, 0x71),
            c(0xc5, 0x96, 0x34),
            c(0x6c, 0x9f, 0xeb),
            c(0xbf, 0x84, 0xd1),
            c(0x1b, 0xb3, 0xba),
            c(0xb7, 0xbe, 0xc7),
            c(0x6b, 0x74, 0x82),
            c(0xff, 0xa0, 0x9c),
            c(0x89, 0xd2, 0x93),
            c(0xe3, 0xb6, 0x62),
            c(0x95, 0xc0, 0xff),
            c(0xdd, 0xa6, 0xee),
            c(0x56, 0xd3, 0xda),
            c(0xed, 0xf2, 0xf9),
        ],
    },
    Builtin {
        name: "Aterm Day",
        appearance: Appearance::Light,
        description: "aterm's signature day: soft paper-white, deep ink-blue text and a cobalt cursor",
        pair: Some("Aterm Night"),
        signature: true,
        source: None,
        fg: c(0x20, 0x27, 0x33),
        bg: c(0xf8, 0xfa, 0xfd),
        cursor: c(0x29, 0x71, 0xc6),
        selection: c(0xb3, 0xce, 0xeb),
        ansi: [
            c(0x28, 0x2e, 0x38),
            c(0xb6, 0x4f, 0x4b),
            c(0x29, 0x81, 0x44),
            c(0x9e, 0x66, 0x02),
            c(0x3c, 0x72, 0xc0),
            c(0x97, 0x57, 0xa5),
            c(0x03, 0x7e, 0x89),
            c(0x5f, 0x63, 0x6c),
            c(0x81, 0x86, 0x8f),
            c(0x98, 0x34, 0x32),
            c(0x14, 0x6d, 0x34),
            c(0x7d, 0x50, 0x02),
            c(0x24, 0x59, 0xa5),
            c(0x7b, 0x3d, 0x89),
            c(0x01, 0x67, 0x70),
            c(0x1c, 0x22, 0x2b),
        ],
    },
    Builtin {
        name: "Hearth",
        appearance: Appearance::Dark,
        description: "embers after dark: a warm umber ground, parchment ink and an amber cursor",
        pair: Some("Linen"),
        signature: true,
        source: None,
        fg: c(0xea, 0xdc, 0xc5),
        bg: c(0x1e, 0x15, 0x10),
        cursor: c(0xf4, 0xa4, 0x37),
        selection: c(0x54, 0x32, 0x23),
        ansi: [
            c(0x2f, 0x24, 0x1e),
            c(0xe4, 0x7d, 0x6d),
            c(0x83, 0xae, 0x53),
            c(0xcd, 0x91, 0x30),
            c(0x4f, 0xa8, 0xe1),
            c(0xd7, 0x7c, 0xb1),
            c(0x29, 0xb5, 0xa8),
            c(0xc9, 0xbb, 0xa9),
            c(0x82, 0x71, 0x63),
            c(0xff, 0xa1, 0x92),
            c(0xa4, 0xcd, 0x79),
            c(0xeb, 0xb2, 0x5f),
            c(0x78, 0xc7, 0xfd),
            c(0xf5, 0x9e, 0xcf),
            c(0x5c, 0xd5, 0xc7),
            c(0xf8, 0xf1, 0xe3),
        ],
    },
    Builtin {
        name: "Linen",
        appearance: Appearance::Light,
        description: "sunlit linen: warm cream paper, walnut ink and a rust-red cursor",
        pair: Some("Hearth"),
        signature: true,
        source: None,
        fg: c(0x3c, 0x29, 0x1e),
        bg: c(0xf9, 0xf3, 0xe6),
        cursor: c(0xbf, 0x49, 0x22),
        selection: c(0xdb, 0xc1, 0x9f),
        ansi: [
            c(0x40, 0x2e, 0x25),
            c(0xb4, 0x4f, 0x3d),
            c(0x49, 0x7a, 0x28),
            c(0x9a, 0x62, 0x03),
            c(0x18, 0x72, 0xb0),
            c(0xa9, 0x4e, 0x85),
            c(0x04, 0x7b, 0x76),
            c(0x6e, 0x60, 0x55),
            c(0x91, 0x83, 0x77),
            c(0x9a, 0x33, 0x22),
            c(0x3b, 0x69, 0x19),
            c(0x7e, 0x4f, 0x02),
            c(0x01, 0x5f, 0x98),
            c(0x8e, 0x33, 0x6c),
            c(0x01, 0x69, 0x65),
            c(0x33, 0x22, 0x19),
        ],
    },
    Builtin {
        name: "Dracula",
        appearance: Appearance::Dark,
        description: "vibrant dark theme — the Dracula palette",
        pair: None,
        signature: false,
        source: Some(BuiltinSource {
            project: "dracula/dracula-theme",
            url: "https://github.com/dracula/dracula-theme",
            licence: "MIT",
        }),
        fg: c(0xf8, 0xf8, 0xf2),
        bg: c(0x28, 0x2a, 0x36),
        cursor: c(0xf8, 0xf8, 0xf2),
        selection: c(0x44, 0x47, 0x5a),
        ansi: [
            c(0x21, 0x22, 0x2c),
            c(0xff, 0x55, 0x55),
            c(0x50, 0xfa, 0x7b),
            c(0xf1, 0xfa, 0x8c),
            c(0xbd, 0x93, 0xf9),
            c(0xff, 0x79, 0xc6),
            c(0x8b, 0xe9, 0xfd),
            c(0xf8, 0xf8, 0xf2),
            c(0x62, 0x72, 0xa4),
            c(0xff, 0x6e, 0x6e),
            c(0x69, 0xff, 0x94),
            c(0xff, 0xff, 0xa5),
            c(0xd6, 0xac, 0xff),
            c(0xff, 0x92, 0xdf),
            c(0xa4, 0xff, 0xff),
            c(0xff, 0xff, 0xff),
        ],
    },
    Builtin {
        name: "Nord",
        appearance: Appearance::Dark,
        description: "arctic, north-bluish palette for dark UIs",
        pair: None,
        signature: false,
        source: Some(BuiltinSource {
            project: "nordtheme/nord",
            url: "https://github.com/nordtheme/nord",
            licence: "MIT",
        }),
        fg: c(0xd8, 0xde, 0xe9),
        bg: c(0x2e, 0x34, 0x40),
        cursor: c(0xec, 0xef, 0xf4),
        selection: c(0x4c, 0x56, 0x6a),
        ansi: [
            c(0x3b, 0x42, 0x52),
            c(0xbf, 0x61, 0x6a),
            c(0xa3, 0xbe, 0x8c),
            c(0xeb, 0xcb, 0x8b),
            c(0x81, 0xa1, 0xc1),
            c(0xb4, 0x8e, 0xad),
            c(0x88, 0xc0, 0xd0),
            c(0xe5, 0xe9, 0xf0),
            c(0x59, 0x63, 0x77),
            c(0xbf, 0x61, 0x6a),
            c(0xa3, 0xbe, 0x8c),
            c(0xeb, 0xcb, 0x8b),
            c(0x81, 0xa1, 0xc1),
            c(0xb4, 0x8e, 0xad),
            c(0x8f, 0xbc, 0xbb),
            c(0xec, 0xef, 0xf4),
        ],
    },
    Builtin {
        name: "Tokyo Night",
        appearance: Appearance::Dark,
        description: "deep blue/purple dark theme",
        pair: None,
        signature: false,
        source: Some(BuiltinSource {
            project: "folke/tokyonight.nvim",
            url: "https://github.com/folke/tokyonight.nvim",
            licence: "Apache-2.0",
        }),
        fg: c(0xc0, 0xca, 0xf5),
        bg: c(0x1a, 0x1b, 0x26),
        cursor: c(0x7a, 0xa2, 0xf7),
        selection: c(0x33, 0x46, 0x7c),
        ansi: [
            c(0x15, 0x16, 0x1e),
            c(0xf7, 0x76, 0x8e),
            c(0x9e, 0xce, 0x6a),
            c(0xe0, 0xaf, 0x68),
            c(0x7a, 0xa2, 0xf7),
            c(0xbb, 0x9a, 0xf7),
            c(0x7d, 0xcf, 0xff),
            c(0xa9, 0xb1, 0xd6),
            c(0x7a, 0x82, 0xa8),
            c(0xff, 0x89, 0x9d),
            c(0x9f, 0xe0, 0x44),
            c(0xfa, 0xba, 0x4a),
            c(0x8d, 0xb0, 0xff),
            c(0xc7, 0xa9, 0xff),
            c(0xa4, 0xda, 0xff),
            c(0xc0, 0xca, 0xf5),
        ],
    },
    Builtin {
        name: "Catppuccin Mocha",
        appearance: Appearance::Dark,
        description: "warm, soothing dark theme (Catppuccin mocha)",
        pair: Some("Catppuccin Latte"),
        signature: false,
        source: Some(BuiltinSource {
            project: "catppuccin/catppuccin",
            url: "https://github.com/catppuccin/catppuccin",
            licence: "MIT",
        }),
        fg: c(0xcd, 0xd6, 0xf4),
        bg: c(0x1e, 0x1e, 0x2e),
        cursor: c(0xf5, 0xe0, 0xdc),
        selection: c(0x58, 0x5b, 0x70),
        ansi: [
            c(0x45, 0x47, 0x5a),
            c(0xf3, 0x8b, 0xa8),
            c(0xa6, 0xe3, 0xa1),
            c(0xf9, 0xe2, 0xaf),
            c(0x89, 0xb4, 0xfa),
            c(0xf5, 0xc2, 0xe7),
            c(0x94, 0xe2, 0xd5),
            c(0xba, 0xc2, 0xde),
            c(0x58, 0x5b, 0x70),
            c(0xf7, 0xae, 0xc2),
            c(0xc2, 0xec, 0xbf),
            c(0xfc, 0xd6, 0x82),
            c(0xae, 0xcc, 0xfc),
            c(0xf3, 0x98, 0xda),
            c(0xb1, 0xea, 0xe1),
            c(0xa6, 0xad, 0xc8),
        ],
    },
    Builtin {
        name: "Gruvbox Dark",
        appearance: Appearance::Dark,
        description: "retro-groove dark theme, warm earthy colours",
        pair: Some("Gruvbox Light"),
        signature: false,
        source: Some(BuiltinSource {
            project: "morhetz/gruvbox",
            url: "https://github.com/morhetz/gruvbox",
            licence: "MIT",
        }),
        fg: c(0xeb, 0xdb, 0xb2),
        bg: c(0x28, 0x28, 0x28),
        cursor: c(0xeb, 0xdb, 0xb2),
        selection: c(0x66, 0x5c, 0x54),
        ansi: [
            c(0x3c, 0x38, 0x36),
            c(0xcc, 0x24, 0x1d),
            c(0x98, 0x97, 0x1a),
            c(0xd7, 0x99, 0x21),
            c(0x45, 0x85, 0x88),
            c(0xb1, 0x62, 0x86),
            c(0x68, 0x9d, 0x6a),
            c(0xa8, 0x99, 0x84),
            c(0x92, 0x83, 0x74),
            c(0xfb, 0x49, 0x34),
            c(0xb8, 0xbb, 0x26),
            c(0xfa, 0xbd, 0x2f),
            c(0x83, 0xa5, 0x98),
            c(0xd3, 0x86, 0x9b),
            c(0x8e, 0xc0, 0x7c),
            c(0xeb, 0xdb, 0xb2),
        ],
    },
    Builtin {
        name: "Solarized Dark",
        appearance: Appearance::Dark,
        description: "dark variant of the Solarized precision palette",
        pair: Some("Solarized Light"),
        signature: false,
        source: Some(BuiltinSource {
            project: "altercation/solarized",
            url: "https://github.com/altercation/solarized",
            licence: "MIT",
        }),
        fg: c(0x83, 0x94, 0x96),
        bg: c(0x00, 0x2b, 0x36),
        cursor: c(0x83, 0x94, 0x96),
        selection: c(0x1e, 0x49, 0x55),
        ansi: [
            c(0x07, 0x36, 0x42),
            c(0xdc, 0x32, 0x2f),
            c(0x85, 0x99, 0x00),
            c(0xb5, 0x89, 0x00),
            c(0x26, 0x8b, 0xd2),
            c(0xd3, 0x36, 0x82),
            c(0x2a, 0xa1, 0x98),
            c(0xee, 0xe8, 0xd5),
            c(0x33, 0x5e, 0x69),
            c(0xcb, 0x4b, 0x16),
            c(0x58, 0x6e, 0x75),
            c(0x65, 0x7b, 0x83),
            c(0x83, 0x94, 0x96),
            c(0x6c, 0x71, 0xc4),
            c(0x93, 0xa1, 0xa1),
            c(0xfd, 0xf6, 0xe3),
        ],
    },
    Builtin {
        name: "One Dark",
        appearance: Appearance::Dark,
        description: "Atom One Dark — atom-inspired dark theme",
        pair: None,
        signature: false,
        source: Some(BuiltinSource {
            project: "atom/one-dark-syntax",
            url: "https://github.com/atom/one-dark-syntax",
            licence: "MIT",
        }),
        fg: c(0xab, 0xb2, 0xbf),
        bg: c(0x21, 0x25, 0x2b),
        cursor: c(0xab, 0xb2, 0xbf),
        selection: c(0x3e, 0x44, 0x51),
        ansi: [
            c(0x2c, 0x31, 0x3a),
            c(0xe0, 0x6c, 0x75),
            c(0x98, 0xc3, 0x79),
            c(0xe5, 0xc0, 0x7b),
            c(0x61, 0xaf, 0xef),
            c(0xc6, 0x78, 0xdd),
            c(0x56, 0xb6, 0xc2),
            c(0xab, 0xb2, 0xbf),
            c(0x76, 0x76, 0x76),
            c(0xe0, 0x6c, 0x75),
            c(0x98, 0xc3, 0x79),
            c(0xe5, 0xc0, 0x7b),
            c(0x61, 0xaf, 0xef),
            c(0xc6, 0x78, 0xdd),
            c(0x56, 0xb6, 0xc2),
            c(0xab, 0xb2, 0xbf),
        ],
    },
    Builtin {
        name: "Default Light",
        appearance: Appearance::Light,
        description: "aterm's own light look, the light half of Default",
        pair: Some("Default"),
        signature: false,
        source: None,
        fg: c(0x2b, 0x2d, 0x33),
        bg: c(0xf6, 0xf7, 0xf9),
        cursor: c(0x1a, 0x7f, 0x37),
        selection: c(0xbc, 0xc9, 0xe0),
        ansi: [
            c(0x2b, 0x2d, 0x33),
            c(0xc4, 0x26, 0x2e),
            c(0x1a, 0x7f, 0x37),
            c(0x8a, 0x61, 0x00),
            c(0x0b, 0x5c, 0xc4),
            c(0xa2, 0x26, 0x9e),
            c(0x0f, 0x76, 0x84),
            c(0x6b, 0x72, 0x80),
            c(0x5c, 0x63, 0x70),
            c(0xad, 0x04, 0x2a),
            c(0x04, 0x6c, 0x2f),
            c(0x75, 0x51, 0x03),
            c(0x00, 0x4d, 0xa3),
            c(0x94, 0x07, 0x7a),
            c(0x02, 0x64, 0x70),
            c(0x47, 0x4d, 0x57),
        ],
    },
    Builtin {
        name: "Solarized Light",
        appearance: Appearance::Light,
        description: "light variant of the Solarized precision palette",
        pair: Some("Solarized Dark"),
        signature: false,
        source: Some(BuiltinSource {
            project: "altercation/solarized",
            url: "https://github.com/altercation/solarized",
            licence: "MIT",
        }),
        fg: c(0x58, 0x6e, 0x75),
        bg: c(0xfd, 0xf6, 0xe3),
        cursor: c(0x58, 0x6e, 0x75),
        selection: c(0xcf, 0xc9, 0xb7),
        ansi: [
            c(0x07, 0x36, 0x42),
            c(0xdc, 0x32, 0x2f),
            c(0x85, 0x99, 0x00),
            c(0xb5, 0x89, 0x00),
            c(0x26, 0x8b, 0xd2),
            c(0xd3, 0x36, 0x82),
            c(0x2a, 0xa1, 0x98),
            c(0xbb, 0xb5, 0xa2),
            c(0x00, 0x2b, 0x36),
            c(0xcb, 0x4b, 0x16),
            c(0x58, 0x6e, 0x75),
            c(0x65, 0x7b, 0x83),
            c(0x83, 0x94, 0x96),
            c(0x6c, 0x71, 0xc4),
            c(0x93, 0xa1, 0xa1),
            c(0xee, 0xe8, 0xd5),
        ],
    },
    Builtin {
        name: "Gruvbox Light",
        appearance: Appearance::Light,
        description: "retro-groove light theme, warm earthy colours",
        pair: Some("Gruvbox Dark"),
        signature: false,
        source: Some(BuiltinSource {
            project: "morhetz/gruvbox",
            url: "https://github.com/morhetz/gruvbox",
            licence: "MIT",
        }),
        fg: c(0x3c, 0x38, 0x36),
        bg: c(0xfb, 0xf1, 0xc7),
        cursor: c(0x3c, 0x38, 0x36),
        selection: c(0xbd, 0xae, 0x93),
        ansi: [
            c(0xeb, 0xdb, 0xb2),
            c(0xcc, 0x24, 0x1d),
            c(0x98, 0x97, 0x1a),
            c(0xd7, 0x99, 0x21),
            c(0x45, 0x85, 0x88),
            c(0xb1, 0x62, 0x86),
            c(0x68, 0x9d, 0x6a),
            c(0x7c, 0x6f, 0x64),
            c(0x92, 0x83, 0x74),
            c(0x9d, 0x00, 0x06),
            c(0x79, 0x74, 0x0e),
            c(0xb5, 0x76, 0x14),
            c(0x07, 0x66, 0x78),
            c(0x8f, 0x3f, 0x71),
            c(0x42, 0x7b, 0x58),
            c(0x3c, 0x38, 0x36),
        ],
    },
    Builtin {
        name: "Catppuccin Latte",
        appearance: Appearance::Light,
        description: "warm, soothing light theme (Catppuccin latte)",
        pair: Some("Catppuccin Mocha"),
        signature: false,
        source: Some(BuiltinSource {
            project: "catppuccin/catppuccin",
            url: "https://github.com/catppuccin/catppuccin",
            licence: "MIT",
        }),
        fg: c(0x4c, 0x4f, 0x69),
        bg: c(0xef, 0xf1, 0xf5),
        cursor: c(0xb9, 0x6a, 0x59),
        selection: c(0xac, 0xb0, 0xbe),
        ansi: [
            c(0xbc, 0xc0, 0xcc),
            c(0xd2, 0x0f, 0x39),
            c(0x40, 0xa0, 0x2b),
            c(0xdf, 0x8e, 0x1d),
            c(0x1e, 0x66, 0xf5),
            c(0xea, 0x76, 0xcb),
            c(0x17, 0x92, 0x99),
            c(0x5c, 0x5f, 0x77),
            c(0xac, 0xb0, 0xbe),
            c(0xe7, 0x10, 0x3f),
            c(0x46, 0xb0, 0x2f),
            c(0xe4, 0x99, 0x31),
            c(0x38, 0x78, 0xf6),
            c(0xef, 0x95, 0xd7),
            c(0x19, 0xa1, 0xa8),
            c(0x6c, 0x6f, 0x85),
        ],
    },
    Builtin {
        name: "GitHub Light",
        appearance: Appearance::Light,
        description: "GitHub's light UI palette — crisp on white",
        pair: None,
        signature: false,
        source: Some(BuiltinSource {
            project: "primer/github-vscode-theme",
            url: "https://github.com/primer/github-vscode-theme",
            licence: "MIT",
        }),
        fg: c(0x1f, 0x23, 0x28),
        bg: c(0xff, 0xff, 0xff),
        cursor: c(0x09, 0x69, 0xda),
        selection: c(0x80, 0xcc, 0xff),
        ansi: [
            c(0x24, 0x29, 0x2f),
            c(0xcf, 0x22, 0x2e),
            c(0x11, 0x63, 0x29),
            c(0x4d, 0x2d, 0x00),
            c(0x09, 0x69, 0xda),
            c(0x82, 0x50, 0xdf),
            c(0x1b, 0x7c, 0x83),
            c(0x6e, 0x77, 0x81),
            c(0x57, 0x60, 0x6a),
            c(0xa4, 0x0e, 0x26),
            c(0x1a, 0x7f, 0x37),
            c(0x63, 0x3c, 0x01),
            c(0x21, 0x8b, 0xff),
            c(0xa4, 0x75, 0xf9),
            c(0x31, 0x92, 0xaa),
            c(0x8c, 0x95, 0x9f),
        ],
    },
];

/// Look up a built-in scheme by name (case-insensitive).
#[must_use]
pub fn builtin(name: &str) -> Option<ColorScheme> {
    if name.eq_ignore_ascii_case(DEFAULT_THEME.name) {
        return Some(ColorScheme::default());
    }
    BUILTINS
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case(name))
        .map(Builtin::to_scheme)
}

/// All built-in scheme names (for `--list-themes` and config diagnostics), with `"Default"` first.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn builtin_names() -> Vec<&'static str> {
    let mut v = Vec::with_capacity(BUILTINS.len().min(64) + 1);
    v.push(DEFAULT_THEME.name);
    v.extend(BUILTINS.iter().map(|b| b.name));
    v
}

/// The identity half of a built-in theme: what listings, the picker and the split resolver need without materializing its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinTheme {
    /// Canonical spelling (what `theme = "…"` and listings show).
    pub name: &'static str,
    /// The appearance the theme is drawn for (always the authored one).
    pub appearance: Appearance,
    /// One line for listings and the picker.
    pub description: &'static str,
    /// The same family's built-in for the other appearance, if aterm ships one.
    pub pair: Option<&'static str>,
    /// One of aterm's own signature themes (listed right after Default).
    pub signature: bool,
    /// Where a ported palette comes from, for its licence notice; `None` for the themes designed here.
    pub source: Option<BuiltinSource>,
}

/// The upstream project a built-in port's palette is taken from, and the licence it is used under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinSource {
    /// `owner/repo` on GitHub.
    pub project: &'static str,
    /// The project's home page.
    pub url: &'static str,
    /// The licence's SPDX identifier.
    pub licence: &'static str,
}

/// Identity row for the `Default` scheme.
const DEFAULT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Default",
    appearance: Appearance::Dark,
    description: "aterm's historical dark look: pale grey text and a green cursor",
    pair: Some("Default Light"),
    signature: false,
    source: None,
};

/// The built-in theme a name means (case-insensitive, `Default` included).
#[must_use]
pub fn builtin_theme(name: &str) -> Option<BuiltinTheme> {
    if name.eq_ignore_ascii_case(DEFAULT_THEME.name) {
        return Some(DEFAULT_THEME);
    }
    BUILTINS
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case(name))
        .map(Builtin::identity)
}

/// Every built-in theme's identity, `Default` first, in registry order.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn builtin_theme_list() -> Vec<BuiltinTheme> {
    let mut v = Vec::with_capacity(BUILTINS.len().min(64) + 1);
    v.push(DEFAULT_THEME);
    v.extend(BUILTINS.iter().map(Builtin::identity));
    v
}

/// The built-in paired with `name` for the other appearance (canonical spelling), or `None` when `name` is not a built-in or has no pair.
#[must_use]
pub fn builtin_pair(name: &str) -> Option<&'static str> {
    builtin_theme(name)?.pair
}

/// All built-in schemes as `(name, description)` pairs, `"Default"` first — the data behind `aterm list-themes`.
#[must_use]
pub fn builtin_themes() -> Vec<(&'static str, &'static str)> {
    builtin_theme_list()
        .into_iter()
        .take(64)
        .map(|theme| (theme.name, theme.description))
        .collect()
}

/// Error from parsing a disk theme definition ([`parse_scheme_str`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    /// A line was syntactically malformed (no `=`, bad colour, unknown or duplicate key, over-long text), or a required key was missing.
    Parse {
        /// 1-based line number of the offending line (`0` for a whole-file problem such as a missing required key).
        line: usize,
        /// Short human-readable reason for the failure.
        reason: String,
        /// The key the failure is about, canonically spelled (`foreground`, `color3`), or as written for an unknown key; `None` for a line with no key.
        key: Option<String>,
        /// The other terminal's theme format the file looks like, when it looks like one ([`sniff_foreign_format`]); such a file is converted with `aterm theme import`, never read natively.
        foreign_format: Option<ForeignFormat>,
    },
}

impl ThemeError {
    /// Creates a [`ThemeError::Parse`] at `line` about `key` with no foreign-format hint.
    const fn at(line: usize, key: Option<String>, reason: String) -> Self {
        ThemeError::Parse {
            line,
            reason,
            key,
            foreign_format: None,
        }
    }
}

impl core::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ThemeError::Parse {
                line,
                reason,
                foreign_format,
                ..
            } => {
                f.write_str("theme parse error on line ")?;
                f.write_str(&line.to_string())?;
                f.write_str(": ")?;
                f.write_str(reason)?;
                if let Some(format) = foreign_format {
                    f.write_str(" (this looks like a ")?;
                    f.write_str(format.label())?;
                    f.write_str(" theme, which aterm does not read directly; convert it with `aterm theme import <file>`)")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// Another terminal's theme file format, recognised by its shape so a parse error can say what the file is and how to bring it in (`aterm theme import`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignFormat {
    /// kitty: whitespace-separated `key value` lines (`foreground #c0caf5`).
    Kitty,
    /// Ghostty: hyphenated keys (`cursor-color`) or `palette = N=#hex`.
    Ghostty,
    /// An XML property list that is not a Terminal.app profile (iTerm2 `.itermcolors`).
    Plist,
    /// A Terminal.app profile (`.terminal`): an XML property list with Terminal.app's keys
    /// (`TextColor`, `ANSIBlackColor`, `type` = `Window Settings`).
    TerminalApp,
    /// TOML that is not a WezTerm scheme (Alacritty): `[table]` headers or quoted colour values.
    Toml,
    /// A WezTerm colour scheme: TOML with a `[metadata]` table, or a `[colors]` table
    /// holding `foreground`, `ansi`, `brights` or their kin.
    WezTerm,
    /// JSON (Windows Terminal, VS Code): a `{ … }` document.
    Json,
}

impl ForeignFormat {
    /// The user-facing name of the format.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ForeignFormat::Kitty => "kitty",
            ForeignFormat::Ghostty => "Ghostty",
            ForeignFormat::Plist => "property-list (iTerm2 .itermcolors)",
            ForeignFormat::TerminalApp => "property-list (Terminal.app .terminal)",
            ForeignFormat::Toml => "TOML (Alacritty)",
            ForeignFormat::WezTerm => "TOML (WezTerm)",
            ForeignFormat::Json => "JSON (Windows Terminal)",
        }
    }
}

/// Guess which other terminal's theme format `text` is written in, or `None` when it does not look like any of them.
#[must_use]
pub fn sniff_foreign_format(text: &str) -> Option<ForeignFormat> {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("<?xml")
        || head.starts_with("<!DOCTYPE plist")
        || head.starts_with("<plist")
    {
        return Some(
            if TERMINAL_APP_MARKERS
                .iter()
                .any(|marker| text.contains(marker))
            {
                ForeignFormat::TerminalApp
            } else {
                ForeignFormat::Plist
            },
        );
    }
    if head.starts_with('{') {
        return Some(ForeignFormat::Json);
    }
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            // TOML table header: `[colors.primary]`, `[[themes]]`.
            if line.starts_with('[') && line.ends_with(']') {
                return Some(toml_flavour(text));
            }
            let mut tokens = line.split_whitespace();
            if let (Some(key), Some(value)) = (tokens.next(), tokens.next())
                && !key.is_empty()
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && (value.starts_with('#') || value.eq_ignore_ascii_case("none"))
            {
                return Some(ForeignFormat::Kitty);
            }
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if value.starts_with('"') || value.starts_with('\'') || value.starts_with('[') {
            return Some(toml_flavour(text));
        }
        if (key.eq_ignore_ascii_case("palette") && value.contains('=')) || key.contains('-') {
            return Some(ForeignFormat::Ghostty);
        }
    }
    None
}

/// Text only a Terminal.app profile's property list holds: iTerm2 spells its colour keys
/// `Ansi 0 Color` and `Background Color`.
const TERMINAL_APP_MARKERS: [&str; 5] = [
    "<key>ANSI",
    "<key>BackgroundColor</key>",
    "<key>TextColor</key>",
    "<key>ProfileCurrentVersion</key>",
    "<string>Window Settings</string>",
];

/// The keys WezTerm's `[colors]` table holds and Alacritty's never does.
const WEZTERM_COLOUR_KEYS: [&str; 15] = [
    "foreground",
    "background",
    "cursor_bg",
    "cursor_fg",
    "cursor_border",
    "selection_fg",
    "selection_bg",
    "ansi",
    "brights",
    "indexed",
    "tab_bar",
    "split",
    "scrollbar_thumb",
    "visual_bell",
    "compose_cursor",
];

/// Which terminal's theme the TOML `text` is: [`ForeignFormat::WezTerm`] when a table header or
/// key path is `metadata…` or `colors.<a WezTerm colour key>…`, else [`ForeignFormat::Toml`].
fn toml_flavour(text: &str) -> ForeignFormat {
    let wezterm = |path: &str| {
        let mut parts = path.split('.');
        match (parts.next(), parts.next()) {
            (Some("metadata"), _) => true,
            (Some("colors"), Some(key)) => WEZTERM_COLOUR_KEYS.contains(&key),
            _ => false,
        }
    };
    let bare = |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
    let mut table = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            table = bare(
                header
                    .trim_start_matches('[')
                    .split(']')
                    .next()
                    .unwrap_or(""),
            );
            if wezterm(&table) {
                return ForeignFormat::WezTerm;
            }
        } else if let Some((key, _)) = line.split_once('=') {
            let key = bare(key);
            let path = if table.is_empty() {
                key
            } else {
                format!("{table}.{key}")
            };
            if wezterm(&path) {
                return ForeignFormat::WezTerm;
            }
        }
    }
    ForeignFormat::Toml
}

/// Parse a theme colour: `#RRGGBB` (the `#` optional) or `#RGB` (the `#` required, so a bare word such as `bad` or `fed` stays an error rather than a colour), surrounding whitespace ignored.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let hashed = h.starts_with('#');
    let h = h.strip_prefix('#').unwrap_or(h);
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => Some(Rgb::new(
            u8::from_str_radix(h.get(0..2)?, 16).ok()?,
            u8::from_str_radix(h.get(2..4)?, 16).ok()?,
            u8::from_str_radix(h.get(4..6)?, 16).ok()?,
        )),
        3 if hashed => {
            let nibble = |range: core::ops::Range<usize>| {
                u8::from_str_radix(h.get(range)?, 16)
                    .ok()
                    .map(|d| d.saturating_mul(17))
            };
            Some(Rgb::new(nibble(0..1)?, nibble(1..2)?, nibble(2..3)?))
        }
        _ => None,
    }
}

/// Longest `name`, `description` or `author` a theme file may carry, in chars.
pub const MAX_THEME_TEXT_CHARS: usize = 256;

/// A disk theme file as read by [`parse_theme_str`]: the colours plus the identity text that is not part of a [`ColorScheme`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTheme {
    /// The colours, name and appearance.
    pub scheme: ColorScheme,
    /// The file's `description`, if any.
    pub description: Option<String>,
    /// The file's `author`, if any.
    pub author: Option<String>,
    /// Whether the file set `appearance` itself.
    pub appearance_declared: bool,
    /// The type settings the file names ([`ThemeTypography::NONE`] when it names none). aterm.toml keys of the same name win over them.
    pub typography: ThemeTypography,
    /// The file's `bold_is_bright`: a colour rung under aterm.toml's key of the same name.
    pub bold_is_bright: Option<bool>,
    /// The file's `schema` line, if it has one.
    pub schema: Option<u16>,
    /// Keys a newer-schema file sets that this aterm does not know: skipped, at most [`MAX_SKIPPED_THEME_KEYS`] names of at most [`MAX_SKIPPED_THEME_KEY_BYTES`] bytes each.
    pub skipped_keys: Vec<String>,
    /// How many keys were skipped, in full.
    pub skipped_count: u16,
}

impl ParsedTheme {
    /// A theme that carries colours and identity only: no typography, no `bold_is_bright`, no `schema` line and nothing skipped.
    #[must_use]
    pub const fn colours_only(
        scheme: ColorScheme,
        appearance_declared: bool,
        description: Option<String>,
        author: Option<String>,
    ) -> Self {
        Self {
            scheme,
            description,
            author,
            appearance_declared,
            typography: ThemeTypography::NONE,
            bold_is_bright: None,
            schema: None,
            skipped_keys: Vec::new(),
            skipped_count: 0,
        }
    }
}

/// Where a theme's [`Appearance`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceSource {
    /// The author said so (a built-in, or a file's `appearance` key).
    Declared,
    /// The file left it out; the catalog measured its background.
    Measured,
}

/// Lowest foreground-on-background contrast a theme is admitted without a warning ([`ThemeWarning::LowTextContrast`]).
pub const MIN_TEXT_CONTRAST: f64 = 3.0;
/// Lowest selection-on-background contrast a theme is admitted without a warning ([`ThemeWarning::FaintSelection`]).
pub const MIN_SELECTION_CONTRAST: f64 = 1.3;
/// Lowest cursor-on-background contrast a theme is admitted without a warning ([`ThemeWarning::LowCursorContrast`]).
pub const MIN_CURSOR_CONTRAST: f64 = 3.0;

/// Lowest authored `accent_color`-on-background contrast a theme is admitted without a warning ([`ThemeWarning::ChromeBelowFloor`]): the non-text floor every chrome surface raises the accent to where it is drawn (`aterm_messages::tokens::ACCENT_FLOOR` is this constant).
pub const MIN_ACCENT_CONTRAST: f64 = 3.0;
/// Lowest authored `border_color`-on-background contrast admitted without a warning: the seam floor (`aterm_messages::tokens::SEAM_FLOOR` is this constant).
pub const MIN_BORDER_CONTRAST: f64 = 1.5;
/// Lowest authored `search_match_color`-on-background contrast admitted without a warning: the match tint's rise (`aterm_messages::tokens::MATCH_RISE`).
pub const MIN_SEARCH_MATCH_CONTRAST: f64 = 1.5;
/// Least `OkLab` distance between an authored `search_match_color` and the selection band admitted without a warning ([`ThemeWarning::SearchMatchLikeSelection`]): the current match is the selection, so a tint that close makes every match look current (`aterm_messages::tokens::MATCH_DISTINCT_OKLAB`).
pub const MIN_SEARCH_MATCH_DISTANCE: f64 = 0.06;

/// A problem a theme file is admitted with: it still loads, but the listing and picker say what will be hard to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeWarning {
    /// Foreground on background below [`MIN_TEXT_CONTRAST`].
    LowTextContrast {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// The selection band on background below [`MIN_SELECTION_CONTRAST`]: a selection is hard to see.
    FaintSelection {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// An ANSI slot is exactly the background, so text in that colour is invisible.
    AnsiMatchesBackground {
        /// The slot, `0..16`.
        slot: u8,
    },
    /// An explicit cursor colour on background below [`MIN_CURSOR_CONTRAST`].
    LowCursorContrast {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// An authored chrome colour on the background below its floor. aterm still draws it, raised to the floor on each surface.
    ChromeBelowFloor {
        /// The chrome key.
        key: ThemeKey,
        /// The measured ratio, in hundredths.
        hundredths: u32,
        /// The floor, in hundredths.
        floor_hundredths: u32,
    },
    /// The authored `search_match_color` sits within [`MIN_SEARCH_MATCH_DISTANCE`] of the selection band, so every match looks like the current one.
    SearchMatchLikeSelection,
    /// The file declares one appearance and its background measures as the other.
    AppearanceMismatch {
        /// What the file says.
        declared: Appearance,
        /// What its background measures as.
        measured: Appearance,
    },
    /// The file was written for a newer theme schema and sets keys this aterm does not know; they were skipped (the names are [`ThemeMeta::skipped_keys`]).
    SkippedNewerKeys {
        /// The file's `schema`.
        schema: u16,
        /// How many keys were skipped.
        count: u16,
    },
}

/// A contrast ratio in hundredths, rounded down (a ratio is in `[1, 21]`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a WCAG ratio is in [1, 21], so ratio * 100 fits a u32 and is positive"
)]
fn hundredths(ratio: f64) -> u32 {
    (ratio.clamp(0.0, 100.0) * 100.0) as u32
}

impl core::fmt::Display for ThemeWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let ratio =
            |f: &mut core::fmt::Formatter<'_>, h: u32| write!(f, "{}.{:02}:1", h / 100, h % 100);
        match *self {
            Self::LowTextContrast { hundredths } => {
                f.write_str("text on background is only ")?;
                ratio(f, hundredths)?;
                f.write_str(" (below 3:1)")
            }
            Self::FaintSelection { hundredths } => {
                f.write_str("selection on background is only ")?;
                ratio(f, hundredths)?;
                f.write_str(" (below 1.3:1), so a selection is hard to see")
            }
            Self::AnsiMatchesBackground { slot } => {
                write!(
                    f,
                    "color{slot} equals the background, so text in it is invisible"
                )
            }
            Self::LowCursorContrast { hundredths } => {
                f.write_str("cursor on background is only ")?;
                ratio(f, hundredths)?;
                f.write_str(" (below 3:1)")
            }
            Self::ChromeBelowFloor {
                key,
                hundredths,
                floor_hundredths,
            } => {
                write!(f, "{} is ", theme_key_spec(key).name)?;
                ratio(f, hundredths)?;
                f.write_str(" on the background; aterm raises it to ")?;
                ratio(f, floor_hundredths)?;
                f.write_str(" where it is drawn")
            }
            Self::SearchMatchLikeSelection => f.write_str(
                "search_match_color is too close to the selection, so every match looks like the current one",
            ),
            Self::AppearanceMismatch { declared, measured } => write!(
                f,
                "declares appearance = {} but its background is {}",
                appearance_word(declared),
                appearance_word(measured)
            ),
            Self::SkippedNewerKeys { schema, count } => write!(
                f,
                "written for theme schema {schema}; {count} key(s) this aterm does not know were skipped"
            ),
        }
    }
}

/// The colour-identity half of a theme: what a listing, the picker and the split resolver show and pair by, separate from the colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeMeta {
    /// What a person sees: a file's `name =`, else its id (the file stem, which stays what config writes); a built-in's canonical name.
    pub display_name: String,
    /// One line for listings and the picker.
    pub description: Option<String>,
    /// The file's `author`; `None` for built-ins.
    pub author: Option<String>,
    /// The appearance the theme is drawn for.
    pub appearance: Appearance,
    /// Whether [`Self::appearance`] was declared or measured.
    pub appearance_source: AppearanceSource,
    /// The theme for the other appearance (a built-in's family pair).
    pub pair: Option<String>,
    /// Non-fatal admission problems, in a fixed order.
    pub warnings: Vec<ThemeWarning>,
    /// The keys a newer-schema file sets that this aterm skipped (bounded as [`ParsedTheme::skipped_keys`]); empty for a file read in full.
    pub skipped_keys: Vec<String>,
}

impl ThemeMeta {
    /// A built-in's identity.
    #[must_use]
    pub fn of_builtin(theme: BuiltinTheme) -> Self {
        Self {
            display_name: theme.name.to_string(),
            description: Some(theme.description.to_string()),
            author: None,
            appearance: theme.appearance,
            appearance_source: AppearanceSource::Declared,
            pair: theme.pair.map(str::to_string),
            warnings: Vec::new(),
            skipped_keys: Vec::new(),
        }
    }

    /// Admit a parsed theme file whose id is `id` (its file stem).
    #[must_use]
    pub fn admit(parsed: &mut ParsedTheme, id: &str, measured: Appearance) -> Self {
        let scheme = &mut parsed.scheme;
        let appearance_source = if parsed.appearance_declared {
            AppearanceSource::Declared
        } else {
            scheme.appearance = measured;
            AppearanceSource::Measured
        };
        let display_name = if scheme.name.trim().is_empty() {
            id.to_string()
        } else {
            scheme.name.clone()
        };
        let mut warnings = admission_warnings(scheme, measured);
        let appearance = scheme.appearance;
        if parsed.skipped_count > 0 {
            warnings.push(ThemeWarning::SkippedNewerKeys {
                schema: parsed.schema.unwrap_or(THEME_SCHEMA_VERSION),
                count: parsed.skipped_count,
            });
        }
        Self {
            display_name,
            description: parsed.description.clone(),
            author: parsed.author.clone(),
            appearance,
            appearance_source,
            pair: None,
            warnings,
            skipped_keys: parsed.skipped_keys.clone(),
        }
    }
}

/// A whole admitted theme: its identity, its colours, its typography and its `bold_is_bright`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePackage {
    /// Identity: name, appearance (declared or measured), pair, warnings.
    pub meta: ThemeMeta,
    /// The colours.
    pub colors: ColorScheme,
    /// The type settings ([`ThemeTypography::NONE`] for none).
    pub typography: ThemeTypography,
    /// The theme's `bold_is_bright`, a colour rung under the config's key.
    pub bold_is_bright: Option<bool>,
}

impl ThemePackage {
    /// Admit a parsed theme file whose id is `id`, as [`ThemeMeta::admit`] does (`measured` is its background's appearance by the caller's classifier).
    #[must_use]
    pub fn admit(mut parsed: ParsedTheme, id: &str, measured: Appearance) -> Self {
        let meta = ThemeMeta::admit(&mut parsed, id, measured);
        Self {
            meta,
            colors: parsed.scheme,
            typography: parsed.typography,
            bold_is_bright: parsed.bold_is_bright,
        }
    }

    /// The `Default` built-in as a package (what an unresolvable selection draws).
    #[must_use]
    pub fn default_theme() -> Self {
        Self {
            meta: ThemeMeta::of_builtin(DEFAULT_THEME),
            colors: ColorScheme::default(),
            typography: ThemeTypography::NONE,
            bold_is_bright: None,
        }
    }

    /// A built-in theme as a package: built-ins carry no typography and no `bold_is_bright`.
    #[must_use]
    pub fn of_builtin(theme: BuiltinTheme) -> Option<Self> {
        Some(Self {
            meta: ThemeMeta::of_builtin(theme),
            colors: builtin(theme.name)?,
            typography: ThemeTypography::NONE,
            bold_is_bright: None,
        })
    }
}

/// Every [`ThemeWarning`] `scheme` earns, in a fixed order: text, selection, ANSI slots (ascending), cursor, the authored chrome colours (accent, border, search match, then the search match's distance from the selection), appearance.
#[must_use]
pub fn admission_warnings(scheme: &ColorScheme, measured: Appearance) -> Vec<ThemeWarning> {
    let bg = scheme.background;
    let report = contrast_report(scheme);
    let below = |pair: ContrastPair| {
        report
            .iter()
            .find(|row| row.pair == pair && row.below_minimum())
            .map(|row| row.hundredths)
    };
    let mut warnings = Vec::new();
    if let Some(hundredths) = below(ContrastPair::TextOnBackground) {
        warnings.push(ThemeWarning::LowTextContrast { hundredths });
    }
    if let Some(hundredths) = below(ContrastPair::SelectionOnBackground) {
        warnings.push(ThemeWarning::FaintSelection { hundredths });
    }
    for (slot, colour) in (0u8..16).zip(scheme.ansi.iter()) {
        if *colour == bg {
            warnings.push(ThemeWarning::AnsiMatchesBackground { slot });
        }
    }
    if let Some(hundredths) = below(ContrastPair::CursorOnBackground) {
        warnings.push(ThemeWarning::LowCursorContrast { hundredths });
    }
    for (pair, key) in [
        (ContrastPair::AccentOnBackground, ThemeKey::AccentColor),
        (ContrastPair::BorderOnBackground, ThemeKey::BorderColor),
        (
            ContrastPair::SearchMatchOnBackground,
            ThemeKey::SearchMatchColor,
        ),
    ] {
        if let Some(row) = report
            .iter()
            .find(|row| row.pair == pair && row.below_minimum())
        {
            warnings.push(ThemeWarning::ChromeBelowFloor {
                key,
                hundredths: row.hundredths,
                floor_hundredths: row.minimum.unwrap_or(0),
            });
        }
    }
    if let Some(tint) = scheme.chrome.search_match
        && oklab_distance(tint, scheme.selection_band()) < MIN_SEARCH_MATCH_DISTANCE
    {
        warnings.push(ThemeWarning::SearchMatchLikeSelection);
    }
    if scheme.appearance != measured {
        warnings.push(ThemeWarning::AppearanceMismatch {
            declared: scheme.appearance,
            measured,
        });
    }
    warnings
}

/// Two colours of a theme whose contrast [`contrast_report`] measures: the first is drawn on the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContrastPair {
    /// The foreground on the background.
    TextOnBackground,
    /// ANSI slot `0..16` on the background.
    AnsiOnBackground(u8),
    /// The selection band on the background: can a selection be seen.
    SelectionOnBackground,
    /// The foreground on the selection band.
    TextOnSelection,
    /// The theme's `selection_foreground` on the selection band.
    SelectionTextOnSelection,
    /// The cursor on the background.
    CursorOnBackground,
    /// The authored `accent_color` on the background.
    AccentOnBackground,
    /// The authored `border_color` on the background.
    BorderOnBackground,
    /// The authored `search_match_color` on the background.
    SearchMatchOnBackground,
}

impl ContrastPair {
    /// The two colours as theme keys: `(drawn, on)`, an ANSI slot as `color<N>`.
    #[must_use]
    pub fn keys(self) -> (String, &'static str) {
        let key = |key: ThemeKey| theme_key_spec(key).name.to_string();
        match self {
            Self::TextOnBackground => (key(ThemeKey::Foreground), "background"),
            Self::AnsiOnBackground(slot) => (
                theme_key_spec(ThemeKey::Palette).spelling(Some(slot)),
                "background",
            ),
            Self::SelectionOnBackground => (key(ThemeKey::Selection), "background"),
            Self::TextOnSelection => (key(ThemeKey::Foreground), "selection"),
            Self::SelectionTextOnSelection => (key(ThemeKey::SelectionForeground), "selection"),
            Self::CursorOnBackground => (key(ThemeKey::Cursor), "background"),
            Self::AccentOnBackground => (key(ThemeKey::AccentColor), "background"),
            Self::BorderOnBackground => (key(ThemeKey::BorderColor), "background"),
            Self::SearchMatchOnBackground => (key(ThemeKey::SearchMatchColor), "background"),
        }
    }
}

/// One measured pair of a [`contrast_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContrastRow {
    /// What was measured.
    pub pair: ContrastPair,
    /// The WCAG contrast ratio, in hundredths (rounded down).
    pub hundredths: u32,
    /// The admission bar, in hundredths, when a theme is warned below it ([`admission_warnings`]); `None` for a pair measured for information.
    pub minimum: Option<u32>,
    /// Whether the ratio is below the admission bar (`None` with no bar).
    below: Option<bool>,
}

impl ContrastRow {
    /// Whether this pair is below its admission bar (never, with no bar).
    #[inline]
    #[must_use]
    pub const fn below_minimum(&self) -> bool {
        match self.below {
            Some(b) => b,
            None => false,
        }
    }
}

/// Every contrast a theme's legibility rests on, in a fixed order: text on background, each ANSI slot on background, the selection band on background, text on the selection (and `selection_foreground` on it, when set), the cursor on background, then each authored chrome colour on the background (accent, border, search match).
#[must_use]
pub fn contrast_report(scheme: &ColorScheme) -> Vec<ContrastRow> {
    let bg = scheme.background;
    let band = scheme.selection_band();
    let row = |pair: ContrastPair, a: Rgb, b: Rgb, minimum: Option<f64>| {
        let ratio = a.contrast(b);
        ContrastRow {
            pair,
            hundredths: hundredths(ratio),
            minimum: minimum.map(hundredths),
            below: minimum.map(|min| ratio < min),
        }
    };
    let mut rows = Vec::with_capacity(22);
    rows.push(row(
        ContrastPair::TextOnBackground,
        scheme.foreground,
        bg,
        Some(MIN_TEXT_CONTRAST),
    ));
    for (slot, colour) in (0u8..16).zip(scheme.ansi.iter()) {
        rows.push(row(ContrastPair::AnsiOnBackground(slot), *colour, bg, None));
    }
    rows.push(row(
        ContrastPair::SelectionOnBackground,
        band,
        bg,
        Some(MIN_SELECTION_CONTRAST),
    ));
    rows.push(row(
        ContrastPair::TextOnSelection,
        scheme.foreground,
        band,
        None,
    ));
    if let Some(selection_fg) = scheme.selection_fg {
        rows.push(row(
            ContrastPair::SelectionTextOnSelection,
            selection_fg,
            band,
            None,
        ));
    }
    rows.push(row(
        ContrastPair::CursorOnBackground,
        scheme.cursor.unwrap_or(scheme.foreground),
        bg,
        scheme.cursor.map(|_| MIN_CURSOR_CONTRAST),
    ));
    for (pair, colour, minimum) in [
        (
            ContrastPair::AccentOnBackground,
            scheme.chrome.accent,
            MIN_ACCENT_CONTRAST,
        ),
        (
            ContrastPair::BorderOnBackground,
            scheme.chrome.border,
            MIN_BORDER_CONTRAST,
        ),
        (
            ContrastPair::SearchMatchOnBackground,
            scheme.chrome.search_match,
            MIN_SEARCH_MATCH_CONTRAST,
        ),
    ] {
        if let Some(colour) = colour {
            rows.push(row(pair, colour, bg, Some(minimum)));
        }
    }
    rows
}

/// Returns the Euclidean `OkLab` distance between two colours.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "Björn Ottosson's published `OkLab` matrices, digit for digit"
)]
fn oklab_distance(a: Rgb, b: Rgb) -> f64 {
    let lin = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lab = |c: Rgb| {
        let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        [
            0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
        ]
    };
    let (p, q) = (lab(a), lab(b));
    (p[0] - q[0]).hypot(p[1] - q[1]).hypot(p[2] - q[2])
}

/// A theme as the catalog holds it, under the id `theme = "<id>"` names: a built-in, or a themes-folder file that loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTheme {
    /// The id, canonically spelled (a built-in's name, a file's stem).
    pub id: String,
    /// The colours and identity text, as a theme file would carry them.
    pub theme: ParsedTheme,
    /// Its identity: appearance (declared or measured), pair, warnings.
    pub meta: ThemeMeta,
}

impl CatalogTheme {
    /// A built-in theme, `None` for a name that is not one.
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        let identity = builtin_theme(name)?;
        let scheme = builtin(identity.name)?;
        Some(Self {
            id: identity.name.to_string(),
            theme: ParsedTheme::colours_only(
                scheme,
                true,
                Some(identity.description.to_string()),
                None,
            ),
            meta: ThemeMeta::of_builtin(identity),
        })
    }
}

/// One key of the theme format, by what it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeKey {
    /// `name`: the display name.
    Name,
    /// `appearance`: dark or light.
    Appearance,
    /// `description`: one line for listings.
    Description,
    /// `author`.
    Author,
    /// `foreground` / `fg`.
    Foreground,
    /// `background` / `bg`.
    Background,
    /// `cursor` / `cursor_color`.
    Cursor,
    /// `selection` / `selection_color`.
    Selection,
    /// `selection_foreground`.
    SelectionForeground,
    /// `color<N>` / `palette<N>` / `ansi<N>`: ANSI slot `N`.
    Palette,
    /// `palette_generate`: derive slots 16-255 from the ANSI colours.
    PaletteGenerate,
    /// `accent_color`: the chrome accent.
    AccentColor,
    /// `active_tab_color`: the selected tab's background.
    ActiveTabColor,
    /// `border_color`: dividers and the chrome bands' seams.
    BorderColor,
    /// `search_match_color`: find's highlight on the other matches.
    SearchMatchColor,
    /// `schema`: the theme format version the file was written for.
    Schema,
    /// `bold_is_bright`: whether bold text in ANSI colours 0-7 uses 8-15.
    BoldIsBright,
    /// `font_family`: the regular face's family.
    FontFamily,
    /// `font_family_bold`.
    FontFamilyBold,
    /// `font_family_italic`.
    FontFamilyItalic,
    /// `font_family_bold_italic`.
    FontFamilyBoldItalic,
    /// `fallback_fonts`: families tried for characters the primary lacks.
    FallbackFonts,
    /// `symbol_font`: the family for symbols and icons.
    SymbolFont,
    /// `emoji_font`: the family for colour emoji.
    EmojiFont,
    /// `line_height`: the line height multiplier.
    LineHeight,
    /// `ligatures`: whether programming ligatures are drawn.
    Ligatures,
    /// `font_features`: OpenType feature specs.
    FontFeatures,
    /// `font_synthetic_style`: whether missing bold/italic faces are synthesized.
    FontSyntheticStyle,
    /// `font_weight`: the regular face's weight.
    FontWeight,
    /// `font_size_scale`: the text size multiplier.
    FontSizeScale,
}

impl ThemeKey {
    /// This key's row of [`THEME_KEYS`].
    #[inline]
    #[must_use]
    pub const fn row(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Description => 1,
            Self::Author => 2,
            Self::Appearance => 3,
            Self::Schema => 4,
            Self::Foreground => 5,
            Self::Background => 6,
            Self::Cursor => 7,
            Self::Selection => 8,
            Self::SelectionForeground => 9,
            Self::Palette => 10,
            Self::PaletteGenerate => 11,
            Self::BoldIsBright => 12,
            Self::AccentColor => 13,
            Self::ActiveTabColor => 14,
            Self::BorderColor => 15,
            Self::SearchMatchColor => 16,
            Self::FontFamily => 17,
            Self::FontFamilyBold => 18,
            Self::FontFamilyItalic => 19,
            Self::FontFamilyBoldItalic => 20,
            Self::FallbackFonts => 21,
            Self::SymbolFont => 22,
            Self::EmojiFont => 23,
            Self::LineHeight => 24,
            Self::Ligatures => 25,
            Self::FontFeatures => 26,
            Self::FontSyntheticStyle => 27,
            Self::FontWeight => 28,
            Self::FontSizeScale => 29,
        }
    }
}

/// What a theme key's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeValueType {
    /// A colour: `#RRGGBB` (the `#` optional) or `#RGB` ([`parse_hex`]).
    Colour,
    /// One line of text: at most [`MAX_THEME_TEXT_CHARS`] chars and no control characters; a ` #` or ` ;` in it starts an inline comment.
    Text,
    /// One of [`APPEARANCE_VALUES`], case-insensitive.
    Appearance,
    /// One font family name, never a path or a font file ([`crate::scheme_typography::parse_font_name`]).
    FontName,
    /// A comma-separated list of font family names, `1..=` [`MAX_THEME_FONT_LIST`] entries, none empty.
    FontNameList,
    /// `true` or `false`, case-insensitive.
    Flag,
    /// A decimal with at most three fraction digits, within `[min, max]` (thousandths): `1.15` is 1150.
    Milli {
        /// The least value, in thousandths.
        min: u16,
        /// The greatest value, in thousandths.
        max: u16,
    },
    /// A whole number within `[min, max]`.
    Integer {
        /// The least value.
        min: u16,
        /// The greatest value.
        max: u16,
    },
    /// OpenType feature specs (`tag`, `+tag`, `-tag`, `tag=N`) separated by commas or spaces, `1..=`[`MAX_THEME_FEATURES`] of them.
    FeatureList,
    /// The theme format version: a whole number from 1.
    SchemaVersion,
}

impl ThemeValueType {
    /// The type's name in `aterm theme schema`.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Colour => "colour",
            Self::Text => "text",
            Self::Appearance => "appearance",
            Self::FontName => "font name",
            Self::FontNameList => "font list",
            Self::Flag => "true|false",
            Self::Milli { .. } => "number",
            Self::Integer { .. } => "integer",
            Self::FeatureList => "feature list",
            Self::SchemaVersion => "schema version",
        }
    }
}

/// Which part of a theme a key belongs to: what `aterm theme schema` and the manual group the keys by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKeyGroup {
    /// Who the theme is: its name, description, author and appearance.
    Identity,
    /// The terminal's colours: foreground, background, cursor, selection and the ANSI palette.
    Colour,
    /// The optional colours of aterm's own chrome.
    Chrome,
    /// The optional type settings: font families, line height, ligatures, features, weight and size scale. aterm.toml keys of the same name win.
    Typography,
}

impl ThemeKeyGroup {
    /// The group's name in `aterm theme schema`.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Colour => "colour",
            Self::Chrome => "chrome",
            Self::Typography => "typography",
        }
    }
}

/// The values the `appearance` key takes, as written (case-insensitive).
pub const APPEARANCE_VALUES: [&str; 2] = ["dark", "light"];

/// How many ANSI slots a theme sets (`color0` through `color15`).
pub const THEME_PALETTE_SLOTS: u8 = 16;

/// One row of [`THEME_KEYS`]: everything the parser and `aterm theme schema` know about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeKeySpec {
    /// What the key sets.
    pub key: ThemeKey,
    /// The canonical spelling.
    pub name: &'static str,
    /// The other spellings the parser accepts (stems, for an indexed key).
    pub aliases: &'static [&'static str],
    /// The value's type.
    pub value: ThemeValueType,
    /// Whether a theme without this key is refused.
    pub required: bool,
    /// For an indexed key, how many slots it has (`0..indexed`); `0` for a plain key.
    pub indexed: u8,
    /// The part of the theme the key belongs to.
    pub group: ThemeKeyGroup,
    /// The aterm.toml key that overrides this one when set (its spelling there); `None` for an identity key, which aterm.toml cannot set.
    pub config_key: Option<&'static str>,
    /// What leaving the key out means.
    pub when_omitted: &'static str,
    /// One line on what the key does.
    pub description: &'static str,
    /// A value that parses, for templates and the schema.
    pub example: &'static str,
}

impl ThemeKeySpec {
    /// The key as written for slot `index` (`color3`), or the plain name.
    #[must_use]
    pub fn spelling(&self, index: Option<u8>) -> String {
        let mut out = String::from(self.name);
        if let Some(index) = index {
            out.push_str(&index.to_string());
        }
        out
    }
}

/// THE table of theme keys: [`parse_theme_str`] reads keys by it, and `aterm theme schema` and the `aterm theme new` template are printed from it, so neither can describe a key the parser does not read the same way.
pub const THEME_KEYS: [ThemeKeySpec; 30] = [
    ThemeKeySpec {
        key: ThemeKey::Name,
        name: "name",
        aliases: &[],
        value: ThemeValueType::Text,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Identity,
        config_key: None,
        when_omitted: "the file name without .conf is shown",
        description: "The name listings and the picker show. The file name stays the id that theme = \"<id>\" names.",
        example: "Harbour Night",
    },
    ThemeKeySpec {
        key: ThemeKey::Description,
        name: "description",
        aliases: &[],
        value: ThemeValueType::Text,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Identity,
        config_key: None,
        when_omitted: "no description",
        description: "One line that listings and the picker show under the name.",
        example: "Deep navy with warm accents",
    },
    ThemeKeySpec {
        key: ThemeKey::Author,
        name: "author",
        aliases: &[],
        value: ThemeValueType::Text,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Identity,
        config_key: None,
        when_omitted: "no author",
        description: "Who made the theme.",
        example: "Jane Doe",
    },
    ThemeKeySpec {
        key: ThemeKey::Appearance,
        name: "appearance",
        aliases: &[],
        value: ThemeValueType::Appearance,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Identity,
        config_key: None,
        when_omitted: "measured from the background",
        description: "Whether the theme is drawn for a dark or a light background; a dark:/light: theme split picks by it.",
        example: "dark",
    },
    ThemeKeySpec {
        key: ThemeKey::Schema,
        name: "schema",
        aliases: &[],
        value: ThemeValueType::SchemaVersion,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Identity,
        config_key: None,
        when_omitted: "the file is read as written for this aterm's schema, so an unknown key is refused",
        description: "The theme format version the file was written for. A newer aterm's file loads here with the keys this aterm does not know skipped and listed.",
        example: "1",
    },
    ThemeKeySpec {
        key: ThemeKey::Foreground,
        name: "foreground",
        aliases: &["fg"],
        value: ThemeValueType::Colour,
        required: true,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("foreground"),
        when_omitted: "the theme is refused",
        description: "The default text colour.",
        example: "#c0caf5",
    },
    ThemeKeySpec {
        key: ThemeKey::Background,
        name: "background",
        aliases: &["bg"],
        value: ThemeValueType::Colour,
        required: true,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("background"),
        when_omitted: "the theme is refused",
        description: "The default background, which also fills the window's padding.",
        example: "#1a1b26",
    },
    ThemeKeySpec {
        key: ThemeKey::Cursor,
        name: "cursor",
        aliases: &["cursor_color"],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("cursor_color"),
        when_omitted: "the cursor follows the foreground",
        description: "The cursor colour.",
        example: "#7aa2f7",
    },
    ThemeKeySpec {
        key: ThemeKey::Selection,
        name: "selection",
        aliases: &["selection_color"],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("selection_color"),
        when_omitted: "derived from the background, moved toward the foreground",
        description: "The selection band's background.",
        example: "#283457",
    },
    ThemeKeySpec {
        key: ThemeKey::SelectionForeground,
        name: "selection_foreground",
        aliases: &[],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("selection_foreground"),
        when_omitted: "selected text keeps its own colour",
        description: "The text colour inside the selection.",
        example: "#c0caf5",
    },
    ThemeKeySpec {
        key: ThemeKey::Palette,
        name: "color",
        aliases: &["palette", "ansi"],
        value: ThemeValueType::Colour,
        required: false,
        indexed: THEME_PALETTE_SLOTS,
        group: ThemeKeyGroup::Colour,
        config_key: Some("palette"),
        when_omitted: "the slot's xterm default",
        description: "ANSI slot N: 0-7 black, red, green, yellow, blue, magenta, cyan, white; 8-15 their bright forms.",
        example: "#f7768e",
    },
    ThemeKeySpec {
        key: ThemeKey::PaletteGenerate,
        name: "palette_generate",
        aliases: &[],
        value: ThemeValueType::Flag,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("palette_generate"),
        when_omitted: "slots 16-255 keep the xterm cube and grey ramp",
        description: "Derive the 256-colour cube and grey ramp (slots 16-255) from this theme's colours: its bright row colours the cube, and its darkest ink (color0 or color15) and lightest colour anchor black and white, so dark slots stay dark on light themes too.",
        example: "true",
    },
    ThemeKeySpec {
        key: ThemeKey::BoldIsBright,
        name: "bold_is_bright",
        aliases: &[],
        value: ThemeValueType::Flag,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Colour,
        config_key: Some("bold_is_bright"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Whether bold text in ANSI colours 0-7 is drawn in their bright forms 8-15.",
        example: "true",
    },
    ThemeKeySpec {
        key: ThemeKey::AccentColor,
        name: "accent_color",
        aliases: &[],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Chrome,
        config_key: Some("accent_color"),
        when_omitted: "the cursor when it has colour, else the theme's ANSI blue or cyan",
        description: "The chrome accent: the active-tab rule, Settings' selected rows, the find highlight's hue. Each surface holds it to 3:1 on its own ground.",
        example: "#7aa2f7",
    },
    ThemeKeySpec {
        key: ThemeKey::ActiveTabColor,
        name: "active_tab_color",
        aliases: &[],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Chrome,
        config_key: Some("active_tab_color"),
        when_omitted: "the selected tab is a raised card in the theme's own tones",
        description: "The selected tab's background; its label is black or white by luminance.",
        example: "#2b3a57",
    },
    ThemeKeySpec {
        key: ThemeKey::BorderColor,
        name: "border_color",
        aliases: &[],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Chrome,
        config_key: Some("border_color"),
        when_omitted: "the background stepped 30% toward the foreground",
        description: "Split-pane dividers and the seams under the tab strip and over the find bar; held to 1.5:1 on each.",
        example: "#3b4261",
    },
    ThemeKeySpec {
        key: ThemeKey::SearchMatchColor,
        name: "search_match_color",
        aliases: &[],
        value: ThemeValueType::Colour,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Chrome,
        config_key: Some("search_match_color"),
        when_omitted: "a tint of the background toward the accent, apart from the selection",
        description: "Find's highlight on every match but the current one (the current match is the selection).",
        example: "#3d4f2f",
    },
    ThemeKeySpec {
        key: ThemeKey::FontFamily,
        name: "font_family",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_family"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The terminal text's font family, by name (never a file or a path). display:<id> names one of aterm's built-in display faces.",
        example: "JetBrains Mono",
    },
    ThemeKeySpec {
        key: ThemeKey::FontFamilyBold,
        name: "font_family_bold",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_family_bold"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The family for bold text.",
        example: "JetBrains Mono",
    },
    ThemeKeySpec {
        key: ThemeKey::FontFamilyItalic,
        name: "font_family_italic",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_family_italic"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The family for italic text.",
        example: "JetBrains Mono",
    },
    ThemeKeySpec {
        key: ThemeKey::FontFamilyBoldItalic,
        name: "font_family_bold_italic",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_family_bold_italic"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The family for bold italic text.",
        example: "JetBrains Mono",
    },
    ThemeKeySpec {
        key: ThemeKey::FallbackFonts,
        name: "fallback_fonts",
        aliases: &[],
        value: ThemeValueType::FontNameList,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("fallback_fonts"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Families tried, in order, for a character the main font lacks (comma-separated, at most 8).",
        example: "Sarasa Mono J, Apple Symbols",
    },
    ThemeKeySpec {
        key: ThemeKey::SymbolFont,
        name: "symbol_font",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("symbol_font"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The family for symbols and icons (Nerd Font glyphs).",
        example: "Symbols Nerd Font Mono",
    },
    ThemeKeySpec {
        key: ThemeKey::EmojiFont,
        name: "emoji_font",
        aliases: &[],
        value: ThemeValueType::FontName,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("emoji_font"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The family for colour emoji.",
        example: "Noto Color Emoji",
    },
    ThemeKeySpec {
        key: ThemeKey::LineHeight,
        name: "line_height",
        aliases: &[],
        value: ThemeValueType::Milli {
            min: 800,
            max: 2000,
        },
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("line_height"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Line height as a multiple of the font's own, 0.8 to 2.0.",
        example: "1.15",
    },
    ThemeKeySpec {
        key: ThemeKey::Ligatures,
        name: "ligatures",
        aliases: &[],
        value: ThemeValueType::Flag,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("ligatures"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Whether programming ligatures (-> and != drawn as one glyph) are shown.",
        example: "true",
    },
    ThemeKeySpec {
        key: ThemeKey::FontFeatures,
        name: "font_features",
        aliases: &[],
        value: ThemeValueType::FeatureList,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_features"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "OpenType features: tag, +tag or tag=N turns one on, -tag off (comma or space separated, at most 16).",
        example: "calt, ss02",
    },
    ThemeKeySpec {
        key: ThemeKey::FontSyntheticStyle,
        name: "font_synthetic_style",
        aliases: &[],
        value: ThemeValueType::Flag,
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_synthetic_style"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Whether a missing bold or italic face is synthesized from the regular one.",
        example: "true",
    },
    ThemeKeySpec {
        key: ThemeKey::FontWeight,
        name: "font_weight",
        aliases: &[],
        value: ThemeValueType::Integer { min: 1, max: 1000 },
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_weight"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "The regular text's weight, 1 to 1000 (400 is regular), for a variable font.",
        example: "450",
    },
    ThemeKeySpec {
        key: ThemeKey::FontSizeScale,
        name: "font_size_scale",
        aliases: &[],
        value: ThemeValueType::Milli {
            min: 800,
            max: 1250,
        },
        required: false,
        indexed: 0,
        group: ThemeKeyGroup::Typography,
        config_key: Some("font_size_scale"),
        when_omitted: "your aterm.toml value, else aterm's default",
        description: "Scales the text size, 0.8 to 1.25, without fixing a pixel size: your font size or aterm's default is multiplied by it.",
        example: "1.08",
    },
];

/// The [`THEME_KEYS`] row for `key`.
#[inline]
#[must_use]
pub const fn theme_key_spec(key: ThemeKey) -> &'static ThemeKeySpec {
    &THEME_KEYS[key.row()]
}

const _: () = {
    let mut i = 0;
    while i < THEME_KEYS.len() {
        assert!(
            THEME_KEYS[i].key.row() == i,
            "THEME_KEYS is out of ThemeKey::row order"
        );
        i += 1;
    }
};

/// Read a key as written (any case, any spelling) by [`THEME_KEYS`]: its row's position in the table and, for an indexed key, the slot.
#[must_use]
pub fn lookup_theme_key(written: &str) -> Option<(usize, Option<u8>)> {
    let key = written.trim().to_ascii_lowercase();
    THEME_KEYS.iter().enumerate().find_map(|(row, spec)| {
        let mut spellings = core::iter::once(spec.name).chain(spec.aliases.iter().copied());
        if spec.indexed == 0 {
            return spellings.any(|s| s == key).then_some((row, None));
        }
        spellings.find_map(|stem| {
            let slot = key.strip_prefix(stem)?;
            if slot.is_empty() || !slot.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let slot = slot.parse::<u8>().ok().filter(|n| *n < spec.indexed)?;
            Some((row, Some(slot)))
        })
    })
}

/// The line each key of a parsed theme was set on, as [`parse_theme_located`] returns it, so a report about a key can point at its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeKeyLines {
    /// Per [`THEME_KEYS`] row (plain keys only).
    plain: [Option<usize>; THEME_KEYS.len()],
    /// Per ANSI slot.
    palette: [Option<usize>; THEME_PALETTE_SLOTS as usize],
}

impl ThemeKeyLines {
    /// The 1-based line `key` (slot `index`, for the palette) was set on, or `None` when the file leaves it out.
    #[inline]
    #[must_use]
    pub fn line(&self, key: ThemeKey, index: Option<u8>) -> Option<usize> {
        if key == ThemeKey::Palette {
            return *self.palette.get(usize::from(index?))?;
        }
        *self.plain.get(key.row())?
    }
}

/// Parse a minimal disk theme definition into a [`ColorScheme`]: exactly [`parse_theme_str`] without the identity text.
/// # Errors
/// As [`parse_theme_str`].
pub fn parse_scheme_str(text: &str) -> Result<ColorScheme, ThemeError> {
    parse_theme_str(text).map(|parsed| parsed.scheme)
}

/// Parse a disk theme definition.
/// # Errors
/// Returns [`ThemeError::Parse`] for a malformed line, a bad colour value, an unknown or duplicate key, over-long text, or a missing required `foreground`/`background`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_theme_str(text: &str) -> Result<ParsedTheme, ThemeError> {
    parse_theme_located(text).map(|(parsed, _)| parsed)
}

/// [`parse_theme_str`], and the line each key was set on (for a report that points at a key, such as `aterm theme check`).
/// # Errors
/// As [`parse_theme_str`].
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_theme_located(text: &str) -> Result<(ParsedTheme, ThemeKeyLines), ThemeError> {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    parse_theme_lines(body).map_err(|error| match error {
        ThemeError::Parse {
            line, reason, key, ..
        } => ThemeError::Parse {
            line,
            reason,
            key,
            foreign_format: sniff_foreign_format(text),
        },
    })
}

/// Line parser behind [`parse_theme_located`].
#[cfg_attr(trust_verify, trust::skip)]
fn parse_theme_lines(text: &str) -> Result<(ParsedTheme, ThemeKeyLines), ThemeError> {
    let mut name = String::new();
    let mut description: Option<String> = None;
    let mut author: Option<String> = None;
    let mut appearance: Option<Appearance> = None;
    let mut foreground: Option<Rgb> = None;
    let mut background: Option<Rgb> = None;
    let mut cursor: Option<Rgb> = None;
    let mut selection: Option<Rgb> = None;
    let mut selection_fg: Option<Rgb> = None;
    let mut chrome = ChromeColors::default();
    let mut ansi = ColorScheme::default().ansi;
    let mut lines = ThemeKeyLines {
        plain: [None; THEME_KEYS.len()],
        palette: [None; THEME_PALETTE_SLOTS as usize],
    };
    let mut typography = ThemeTypography::NONE;
    let mut bold_is_bright: Option<bool> = None;
    let mut palette_generate = false;
    let mut schema: Option<u16> = None;
    let mut skipped_keys: Vec<String> = Vec::new();
    let mut skipped_count: u16 = 0;
    let future = declared_schema(text).is_some_and(|n| n > THEME_SCHEMA_VERSION);

    for (idx, raw) in text.lines().enumerate() {
        let lineno = idx.saturating_add(1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(parse_error(lineno, None, "missing '=' in ", line));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = strip_inline_comment(value).trim().to_string();
        if value.is_empty() {
            return Err(parse_error(
                lineno,
                Some(&key),
                "empty value for key ",
                &key,
            ));
        }
        let Some((row, index)) = lookup_theme_key(&key) else {
            if future {
                skipped_count = skipped_count.saturating_add(1);
                if skipped_keys.len() < MAX_SKIPPED_THEME_KEYS {
                    skipped_keys.push(truncate_at_char(
                        &escape_controls(&key),
                        MAX_SKIPPED_THEME_KEY_BYTES,
                    ));
                }
                continue;
            }
            return Err(parse_error(lineno, Some(&key), "unknown key ", &key));
        };
        let spec = &THEME_KEYS[row];
        let canonical = spec.spelling(index);

        let first_line = match index {
            Some(slot) => &mut lines.palette[usize::from(slot)],
            None => &mut lines.plain[row],
        };
        if let Some(first) = *first_line {
            let mut reason = String::from("duplicate ");
            match index {
                Some(slot) => {
                    reason.push_str("colour index ");
                    reason.push_str(&slot.to_string());
                }
                None => {
                    reason.push_str("key ");
                    reason.push_str(&crate::trust_fmt::debug_string(spec.name));
                }
            }
            reason.push_str(" (first set on line ");
            reason.push_str(&first.to_string());
            reason.push(')');
            return Err(ThemeError::at(lineno, Some(canonical), reason));
        }
        *first_line = Some(lineno);

        match spec.value {
            ThemeValueType::Colour => {
                let Some(colour) = parse_hex(&value) else {
                    let mut reason = String::from("invalid #RRGGBB or #RGB colour for ");
                    reason.push_str(&crate::trust_fmt::debug_string(&canonical));
                    reason.push_str(": ");
                    reason.push_str(&crate::trust_fmt::debug_string(&value));
                    return Err(ThemeError::at(lineno, Some(canonical), reason));
                };
                match (spec.key, index) {
                    (ThemeKey::Foreground, _) => foreground = Some(colour),
                    (ThemeKey::Background, _) => background = Some(colour),
                    (ThemeKey::Cursor, _) => cursor = Some(colour),
                    (ThemeKey::Selection, _) => selection = Some(colour),
                    (ThemeKey::SelectionForeground, _) => selection_fg = Some(colour),
                    (ThemeKey::Palette, Some(slot)) => ansi[usize::from(slot)] = colour,
                    (key, _) => {
                        // The chrome keys; the table types no other key as a
                        // colour.
                        if let Some(slot) = chrome.slot_mut(key) {
                            *slot = Some(colour);
                        }
                    }
                }
            }
            ThemeValueType::Text => {
                let text = bounded_text(lineno, spec.name, &value)?;
                match spec.key {
                    ThemeKey::Name => name = text,
                    ThemeKey::Description => description = Some(text),
                    ThemeKey::Author => author = Some(text),
                    _ => {}
                }
            }
            ThemeValueType::FontName => {
                let name = crate::scheme_typography::parse_font_name(&value)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                match spec.key {
                    ThemeKey::FontFamily => typography.font_family = Some(name),
                    ThemeKey::FontFamilyBold => typography.font_family_bold = Some(name),
                    ThemeKey::FontFamilyItalic => typography.font_family_italic = Some(name),
                    ThemeKey::FontFamilyBoldItalic => {
                        typography.font_family_bold_italic = Some(name);
                    }
                    ThemeKey::SymbolFont => typography.symbol_font = Some(name),
                    ThemeKey::EmojiFont => typography.emoji_font = Some(name),
                    _ => {}
                }
            }
            ThemeValueType::FontNameList => {
                let names = crate::scheme_typography::parse_font_list(&value)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                if spec.key == ThemeKey::FallbackFonts {
                    typography.fallback_fonts = Some(names);
                }
            }
            ThemeValueType::Flag => {
                let flag = crate::scheme_typography::parse_flag(&value)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                match spec.key {
                    ThemeKey::BoldIsBright => bold_is_bright = Some(flag),
                    ThemeKey::PaletteGenerate => palette_generate = flag,
                    ThemeKey::Ligatures => typography.ligatures = Some(flag),
                    ThemeKey::FontSyntheticStyle => typography.font_synthetic_style = Some(flag),
                    _ => {}
                }
            }
            ThemeValueType::Milli { min, max } => {
                let milli = crate::scheme_typography::parse_milli(&value, min, max)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                match spec.key {
                    ThemeKey::LineHeight => typography.line_height_milli = Some(milli),
                    ThemeKey::FontSizeScale => typography.font_size_scale_milli = Some(milli),
                    _ => {}
                }
            }
            ThemeValueType::Integer { min, max } => {
                let n = crate::scheme_typography::parse_integer(&value, min, max)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                if spec.key == ThemeKey::FontWeight {
                    typography.font_weight = Some(n);
                }
            }
            ThemeValueType::FeatureList => {
                let specs = crate::scheme_typography::parse_feature_list(&value)
                    .map_err(|reason| value_error(lineno, &canonical, &reason))?;
                if spec.key == ThemeKey::FontFeatures {
                    typography.font_features = Some(specs);
                }
            }
            ThemeValueType::SchemaVersion => {
                schema = Some(
                    crate::scheme_typography::parse_schema(&value)
                        .map_err(|reason| value_error(lineno, &canonical, &reason))?,
                );
            }
            ThemeValueType::Appearance => {
                let word = value.to_ascii_lowercase();
                appearance = Some(match APPEARANCE_VALUES.iter().position(|v| *v == word) {
                    Some(0) => Appearance::Dark,
                    Some(_) => Appearance::Light,
                    None => {
                        return Err(parse_error(
                            lineno,
                            Some(spec.name),
                            "appearance must be dark|light, got ",
                            &value,
                        ));
                    }
                });
            }
        }
    }

    for (row, spec) in THEME_KEYS.iter().enumerate() {
        if spec.required && spec.indexed == 0 && lines.plain[row].is_none() {
            return Err(missing_key(spec.name));
        }
    }
    let (Some(foreground), Some(background)) = (foreground, background) else {
        return Err(missing_key(if foreground.is_none() {
            "foreground"
        } else {
            "background"
        }));
    };

    let parsed = ParsedTheme {
        scheme: ColorScheme {
            name,
            appearance: appearance.unwrap_or_default(),
            foreground,
            background,
            cursor,
            selection,
            selection_fg,
            ansi,
            chrome,
            palette_generate,
        },
        description,
        author,
        appearance_declared: appearance.is_some(),
        typography,
        bold_is_bright,
        schema,
        skipped_keys,
        skipped_count,
    };
    Ok((parsed, lines))
}

/// Returns the value of the first `schema` line, read leniently.
fn declared_schema(text: &str) -> Option<u16> {
    text.lines().find_map(|raw| {
        let line = raw.trim();
        if line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("schema") {
            return None;
        }
        crate::scheme_typography::parse_schema(strip_inline_comment(value)).ok()
    })
}

/// `text` cut to at most `max` bytes at a char boundary.
fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

fn truncate_at_char(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_string()
}

/// Builds a [`ThemeError::Parse`] for a malformed value of `key`.
fn value_error(line: usize, key: &str, reason: &str) -> ThemeError {
    let mut text = String::from("invalid value for ");
    text.push_str(&crate::trust_fmt::debug_string(key));
    text.push_str(": ");
    text.push_str(reason);
    ThemeError::at(line, Some(key.to_string()), text)
}

/// A whole-file [`ThemeError::Parse`] for a required key the file leaves out.
fn missing_key(name: &str) -> ThemeError {
    let mut reason = String::from("missing required key '");
    reason.push_str(name);
    reason.push('\'');
    ThemeError::at(0, Some(name.to_string()), reason)
}

/// Validates a `name` / `description` / `author` value against length and control-character bounds.
fn bounded_text(line: usize, key: &str, value: &str) -> Result<String, ThemeError> {
    let mut reason = String::from("value for key ");
    reason.push_str(&crate::trust_fmt::debug_string(key));
    if value.chars().count() > MAX_THEME_TEXT_CHARS {
        reason.push_str(" is longer than ");
        reason.push_str(&MAX_THEME_TEXT_CHARS.to_string());
        reason.push_str(" characters");
        return Err(ThemeError::at(line, Some(key.to_string()), reason));
    }
    if value.chars().any(char::is_control) {
        reason.push_str(" contains a control character");
        return Err(ThemeError::at(line, Some(key.to_string()), reason));
    }
    Ok(value.to_string())
}

/// Builds a [`ThemeError::Parse`] at `line` with `prefix` followed by `detail` formatted via `debug_string`.
fn parse_error(line: usize, key: Option<&str>, prefix: &str, detail: &str) -> ThemeError {
    let mut reason = String::from(prefix);
    reason.push_str(&crate::trust_fmt::debug_string(detail));
    ThemeError::at(line, key.map(str::to_string), reason)
}

/// Strips a trailing ` #…` or ` ;…` inline comment from `value` while preserving a leading `#`.
fn strip_inline_comment(value: &str) -> &str {
    let mut prev_ws = true;
    for (i, b) in value.bytes().enumerate() {
        if (b == b'#' || b == b';') && prev_ws {
            let prefix = value.get(..i).unwrap_or(value);
            if prefix.trim().is_empty() {
                prev_ws = false;
                continue;
            }
            return prefix;
        }
        prev_ws = b.is_ascii_whitespace();
    }
    value
}

/// How many theme files the themes folder is read for: discovery opens and parses at most this many `.conf` files (the lowest names win), and a writer that adds many (`aterm theme install --all`) warns past it.
pub const MAX_USER_THEME_FILES: usize = 128;

/// The user theme directory: `themes` inside the folder that holds `aterm.toml` ([`crate::dirs::aterm_config_dir`]), so it follows the same XDG / `%APPDATA%` / `HOME` rule as the config on every platform.
#[must_use]
pub fn user_theme_dir() -> Option<std::path::PathBuf> {
    crate::dirs::aterm_config_dir().map(|dir| dir.join("themes"))
}

/// Validate the literal grammar shared by direct loading and the GUI's bounded theme catalog.
pub fn validate_user_theme_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("theme name is empty");
    }
    if name != name.trim() {
        return Err("theme name has leading or trailing whitespace");
    }
    if name == "." || name == ".." {
        return Err("theme name cannot be a path component");
    }
    if name.chars().any(char::is_control) {
        return Err("theme name contains a control character");
    }
    if name
        .chars()
        .any(|character| matches!(character, '/' | '\\'))
    {
        return Err("theme name contains a path separator");
    }
    if name.contains(',') {
        return Err("theme name contains a comma reserved for appearance splits");
    }
    if name.contains(['[', ']']) {
        return Err("theme name contains a bracket reserved for selector syntax");
    }
    if matches!(classify_selector_segment(name), SelectorSegment::Side(..)) {
        return Err("theme name is ambiguous with dark:/light: split syntax");
    }
    Ok(())
}

/// One comma segment of a `theme` value, classified by the single rule every reader of the selector grammar shares ([`ThemeSelector::parse`] and [`validate_user_theme_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorSegment<'a> {
    /// `dark:<name>` / `light:<name>`: the key is case- and space-insensitive, and the name is trimmed (it may be empty).
    Side(Appearance, &'a str),
    /// `<mode>:<name>` whose mode is not an appearance; the mode is trimmed.
    OtherMode(&'a str, &'a str),
    /// A segment with no `:` at all, trimmed.
    Bare(&'a str),
}

/// Classify one comma segment of a `theme` value (see [`SelectorSegment`]).
#[must_use]
pub fn classify_selector_segment(segment: &str) -> SelectorSegment<'_> {
    let Some((mode, name)) = segment.split_once(':') else {
        return SelectorSegment::Bare(segment.trim());
    };
    let mode = mode.trim();
    if mode.eq_ignore_ascii_case("dark") {
        SelectorSegment::Side(Appearance::Dark, name.trim())
    } else if mode.eq_ignore_ascii_case("light") {
        SelectorSegment::Side(Appearance::Light, name.trim())
    } else {
        SelectorSegment::OtherMode(mode, name.trim())
    }
}

/// A parsed `theme` value: one name for every appearance, or a `dark:<name>,light:<name>` split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSelector {
    /// A plain name, trimmed, applied to both appearances.
    Single(String),
    /// A split: any segment's key is `dark` or `light`.
    Split {
        /// The name for a dark OS appearance.
        dark: Option<String>,
        /// The name for a light OS appearance.
        light: Option<String>,
    },
}

/// Why part of a `theme` value is not used as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorIssue {
    /// A side appears more than once; the first one is used.
    DuplicateSide(Appearance),
    /// A `<mode>:<name>` segment in a split whose mode is not dark or light (lower-cased).
    UnknownMode(String),
    /// A side with no name (`dark:`), or a blank value (`None`).
    EmptyName(Option<Appearance>),
    /// A segment in a split with no `<mode>:` at all.
    JunkSegment(String),
}

#[inline]
const fn appearance_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

impl core::fmt::Display for SelectorIssue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DuplicateSide(side) => write!(
                f,
                "theme split mode {} appears more than once",
                appearance_word(*side)
            ),
            Self::UnknownMode(mode) => {
                write!(f, "theme split mode {mode:?} must be dark or light")
            }
            Self::EmptyName(Some(side)) => write!(
                f,
                "theme split mode {} needs a theme name",
                appearance_word(*side)
            ),
            Self::EmptyName(None) => f.write_str("theme must name a built-in or user theme"),
            Self::JunkSegment(segment) => {
                write!(f, "theme split segment {segment:?} needs dark: or light:")
            }
        }
    }
}

impl ThemeSelector {
    /// Parse a `theme` value.
    #[must_use]
    pub fn parse(raw: &str) -> (Self, Vec<SelectorIssue>) {
        let raw = raw.trim();
        let is_split = raw.split(',').any(|segment| {
            matches!(
                classify_selector_segment(segment),
                SelectorSegment::Side(..)
            )
        });
        if !is_split {
            let issues = if raw.is_empty() {
                vec![SelectorIssue::EmptyName(None)]
            } else {
                Vec::new()
            };
            return (Self::Single(raw.to_string()), issues);
        }
        let (mut dark, mut light) = (None, None);
        let mut issues = Vec::new();
        for segment in raw.split(',') {
            match classify_selector_segment(segment) {
                SelectorSegment::Bare(junk) => {
                    issues.push(SelectorIssue::JunkSegment(junk.to_string()));
                }
                SelectorSegment::OtherMode(mode, _) => {
                    issues.push(SelectorIssue::UnknownMode(mode.to_ascii_lowercase()));
                }
                SelectorSegment::Side(side, "") => {
                    issues.push(SelectorIssue::EmptyName(Some(side)));
                }
                SelectorSegment::Side(side, name) => {
                    let slot = match side {
                        Appearance::Dark => &mut dark,
                        Appearance::Light => &mut light,
                    };
                    if slot.is_some() {
                        issues.push(SelectorIssue::DuplicateSide(side));
                    } else {
                        *slot = Some(name.to_string());
                    }
                }
            }
        }
        (Self::Split { dark, light }, issues)
    }

    /// The name this selector draws for `appearance`: a plain name for both; a split's own side; for a side the split leaves out, the named side's built-in pair ([`builtin_pair`], so `dark:Gruvbox Dark` draws Gruvbox Light on a light OS), else the named side itself.
    #[must_use]
    pub fn name_for(&self, appearance: Appearance) -> Option<&str> {
        match self {
            Self::Single(name) => Some(name.as_str()).filter(|name| !name.is_empty()),
            Self::Split { dark, light } => {
                let (own, other) = match appearance {
                    Appearance::Dark => (dark, light),
                    Appearance::Light => (light, dark),
                };
                own.as_deref().or_else(|| {
                    other
                        .as_deref()
                        .map(|named| builtin_pair(named).unwrap_or(named))
                })
            }
        }
    }

    /// The names the value writes, each with the appearance it is written for (`None` for a plain name that serves both), in dark-then-light order.
    #[must_use]
    pub fn named_sides(&self) -> Vec<(Option<Appearance>, &str)> {
        match self {
            Self::Single(name) if name.is_empty() => Vec::new(),
            Self::Single(name) => vec![(None, name.as_str())],
            Self::Split { dark, light } => [(Appearance::Dark, dark), (Appearance::Light, light)]
                .into_iter()
                .filter_map(|(side, name)| Some((Some(side), name.as_deref()?)))
                .collect(),
        }
    }
}

/// What an unset `theme` means, written as the selector it equals: Default on every appearance, as a value that names no theme at all (blank, or a split whose every segment is unusable) also draws. aterm follows the OS only when a `dark:…,light:…` pair is written (Settings' Follow system switch writes `dark:Default,light:Default Light` over an unset theme), so an update never turns an unconfigured terminal light.
pub const UNSET_THEME: &str = "Default";

/// What a selector draws for one appearance, as far as the built-ins can say ([`ThemeSelector::draw_builtin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorDraw {
    /// No name at all: the unset default, [`UNSET_THEME`] on every appearance.
    Unset(ColorScheme),
    /// A built-in, canonically named.
    Builtin(ColorScheme),
    /// A name the built-ins do not know: the caller's catalog (theme files natively, registered themes on the web) resolves it.
    Other(String),
}

impl SelectorDraw {
    /// The unset default: [`UNSET_THEME`], whatever the appearance.
    #[must_use]
    pub fn unset() -> Self {
        Self::Unset(builtin(UNSET_THEME).unwrap_or_default())
    }
}

impl ThemeSelector {
    /// The built-in side of [`name_for`](Self::name_for): no name draws the unset default, a built-in name (case-insensitive) draws that built-in, and any other name is left to the caller's catalog.
    #[must_use]
    pub fn draw_builtin(&self, appearance: Appearance) -> SelectorDraw {
        match self.name_for(appearance) {
            None => SelectorDraw::unset(),
            Some(name) => builtin(name).map_or_else(
                || SelectorDraw::Other(name.to_string()),
                SelectorDraw::Builtin,
            ),
        }
    }
}

#[cfg(test)]
#[path = "scheme_tests.rs"]
mod tests;
