// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named color schemes ("theme palettes") — the SINGLE source of truth for a
//! terminal's colors, the way iTerm2 / Windows Terminal / Alacritty / Kitty do it.
//!
//! A [`ColorScheme`] bundles the foreground/background/cursor/selection chrome AND
//! the full 16-entry ANSI palette. It projects to BOTH sinks the GUI drives:
//!   * the renderer chrome — via [`ColorScheme::to_theme_parts`] (packed `u32`s the
//!     GUI assembles into an `aterm_render::Theme`); and
//!   * the engine indexed palette — via [`ColorScheme::to_color_palette`].
//!
//! It deliberately exposes ONLY [`Rgb`] / packed-`u32` primitives (never an
//! `aterm_render::Theme`), so `aterm-types` stays at the bottom of the crate graph
//! (`aterm-render -> aterm-core -> aterm-types`); the `Theme`/`TerminalConfig`
//! projections live in `aterm-gui`, which already owns both.

use crate::{ColorPalette, Rgb};

/// Maximum bytes admitted from one user theme definition. Shared by direct
/// loaders and GUI catalog discovery so every entry point has the same bound.
pub const MAX_USER_THEME_FILE_BYTES: usize = 256 * 1024;

/// Whether a scheme is meant for a dark or light background. Drives the optional
/// `dark:…,light:…` auto-split, which `aterm-gui` resolves against the window's
/// appearance; carried here so a scheme is self-describing.
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
    /// The DEC color-scheme DSR parameter for this appearance (`1` = dark,
    /// `2` = light), as reported via DSR `CSI ? 996 n` → `CSI ? 997 ; Ps n` and
    /// the DEC mode 2031 push.
    #[must_use]
    pub fn dsr_code(self) -> u8 {
        match self {
            Appearance::Dark => 1,
            Appearance::Light => 2,
        }
    }
}

/// The renderer chrome of a scheme as packed `0x00RRGGBB` values — the exact shape
/// `aterm_render::Theme` wants, but as plain `u32`s so this crate need not depend on
/// the renderer (no crate cycle). The GUI builds the `Theme` from these.
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
    /// Selection highlight bg; `None` derives one from this scheme's own
    /// background and foreground ([`derived_selection`]), so a light theme
    /// that leaves it out gets a light band and a dark one a dark band.
    pub selection: Option<Rgb>,
    /// Text colour inside the selection; `None` keeps each cell's own colour
    /// (the renderer floors it against the selection). A theme file sets it
    /// with `selection_foreground`; no built-in sets it.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_fg: Option<Rgb>,
    /// The 16 ANSI slots: `[0..8)` normal (black, red, green, yellow, blue, magenta,
    /// cyan, white), `[8..16)` their bright variants. THE authoritative ANSI table.
    pub ansi: [Rgb; 16],
}

/// Default's selection colour (`aterm_render::Theme::default().selection`,
/// `#33415E`). Verified by the `default_theme_parts_are_historical` test in
/// this module. A scheme that leaves `selection` as `None` does not get it:
/// it gets [`derived_selection`], which follows its own background.
///
/// Darker, lower-saturation than the old `#264F78`: selected COLOURED text keeps its
/// own fg, and over `#264F78` mid-tone syntax colours dropped below ~2:1 (magenta,
/// comment-gray, blue) — unreadable while selecting git/ls/log output. `#33415E`
/// darkens the highlight so it recedes behind the text (>~3:1) yet still clearly
/// reads as a selection against the `#111318` background. (LLM-judge finding V3.)
const SELECTION_DEFAULT: Rgb = Rgb::new(0x33, 0x41, 0x5E);

impl Default for ColorScheme {
    /// Byte-identical to aterm's historical look: fg `#D0D0D0`, bg `#111318`, cursor
    /// `#50FA7B`, selection `SELECTION_DEFAULT` (`#33415E`), and `ansi[i] == ColorPalette::default_color(i)`
    /// (so an empty config resolves to exactly today's colors).
    fn default() -> Self {
        let mut ansi = [Rgb::new(0, 0, 0); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            // `i` indexes a 16-entry array, so `i <= 15` and `& 0xF` is
            // identity — the mask only proves the narrowing lossless to the
            // Trust gate.
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
        }
    }
}

impl ColorScheme {
    /// Build the engine indexed palette from this scheme: each of the 16 ANSI slots
    /// is applied via [`ColorPalette::set`], leaving indices 16-255 as the computed
    /// xterm cube/ramp. (`set` elides a slot that equals `default_color(i)`, but that
    /// is render-identical: `get(i)` then falls back to the same `default_color(i)`.)
    #[must_use]
    pub fn to_color_palette(&self) -> ColorPalette {
        let mut p = ColorPalette::new();
        for (i, c) in self.ansi.iter().enumerate() {
            // `i` indexes the 16-entry `ansi` array, so `i <= 15` and `& 0xF`
            // is identity — the mask only proves the narrowing lossless to the
            // Trust gate.
            p.set((i & 0xF) as u8, *c);
        }
        p
    }

    /// Project the chrome to packed `0x00RRGGBB` [`ThemeParts`]. `cursor` falls back
    /// to the foreground and `selection` to [`Self::selection_band`] when unset.
    #[must_use]
    pub fn to_theme_parts(&self) -> ThemeParts {
        let pack = |c: Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        ThemeParts {
            fg: pack(self.foreground),
            bg: pack(self.background),
            cursor: pack(self.cursor.unwrap_or(self.foreground)),
            selection: pack(self.selection_band()),
        }
    }

    /// The selection band this scheme paints: its own `selection`, else one
    /// derived from its background and foreground ([`derived_selection`]).
    #[must_use]
    pub fn selection_band(&self) -> Rgb {
        self.selection
            .unwrap_or_else(|| derived_selection(self.foreground, self.background))
    }
}

/// How far an unset selection band sits from the background toward the
/// foreground, in percent: far enough to read as a band (about 1.5:1 on the
/// background for a normal-contrast theme), near enough that text on it keeps
/// most of its contrast.
pub const DERIVED_SELECTION_PERCENT: u16 = 22;

/// The selection band for a scheme that does not name one: `bg` moved
/// [`DERIVED_SELECTION_PERCENT`] of the way toward `fg`, per sRGB channel,
/// rounded. It sits on the background's side of the theme, so a light theme
/// gets a light band, and it follows a background a config overrides.
#[must_use]
pub fn derived_selection(fg: Rgb, bg: Rgb) -> Rgb {
    let mix = |b: u8, f: u8| {
        let toward = DERIVED_SELECTION_PERCENT;
        let away = 100 - toward;
        // At most 255 * 100 + 50, so it fits a u16, and the quotient is at
        // most 255.
        let blended = (u16::from(b) * away + u16::from(f) * toward + 50) / 100;
        u8::try_from(blended).unwrap_or(u8::MAX)
    };
    Rgb::new(mix(bg.r, fg.r), mix(bg.g, fg.g), mix(bg.b, fg.b))
}

/// A compile-time built-in scheme: all-`Rgb` (const-constructible) so the registry
/// lives in `.rodata` with no allocation. Materialized into a [`ColorScheme`] (which
/// owns a `String` name) on lookup.
struct Builtin {
    name: &'static str,
    appearance: Appearance,
    /// One line for listings and the picker.
    description: &'static str,
    /// The same family's built-in for the other appearance, if aterm ships it.
    pair: Option<&'static str>,
    /// One of aterm's own signature themes, which `builtin_names` lists right
    /// after Default and the `signature_themes_meet_their_bars` test holds to
    /// the stricter signature laws.
    signature: bool,
    fg: Rgb,
    bg: Rgb,
    cursor: Rgb,
    selection: Rgb,
    ansi: [Rgb; 16],
}

impl Builtin {
    fn identity(&self) -> BuiltinTheme {
        BuiltinTheme {
            name: self.name,
            appearance: self.appearance,
            description: self.description,
            pair: self.pair,
            signature: self.signature,
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
        }
    }
}

// Shorthand for terse, readable palette tables below.
const fn c(r: u8, g: u8, b: u8) -> Rgb {
    Rgb::new(r, g, b)
}

/// The bundled, well-known schemes (canonical values verified against
/// github.com/mbadolato/iTerm2-Color-Schemes). `"Default"` is NOT here — it is
/// served from [`ColorScheme::default`] so the historical look has one source.
static BUILTINS: &[Builtin] = &[
    // ── aterm's signature themes ──────────────────────────────────────────────
    // Two light/dark pairs designed here in OkLCh rather than ported: the six
    // chromatic hues at least 25° apart at one lightness per tier, their
    // chroma in one band so no hue shouts, every chromatic slot at 4.5:1 on
    // the background, body text at 7:1, color8 (comments, autosuggestions)
    // between 3:1 and 4.5:1, a 1.5:1 selection band and a cursor in the
    // theme's accent hue. `signature_themes_meet_their_bars` holds all of it.
    Builtin {
        name: "Aterm Night",
        appearance: Appearance::Dark,
        description: "aterm's signature night: low-glare blue-black, cool slate ink and a sea-glass cyan cursor",
        pair: Some("Aterm Day"),
        signature: true,
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
    // Palette values verified against github.com/mbadolato/iTerm2-Color-Schemes and
    // each theme's own spec. NOTE: Nord and Catppuccin's canonical files list a
    // near-WHITE selection (their selection-*foreground*); the built-ins carry no
    // selection foreground (selected text keeps its own colour, which the renderer
    // floors to 4.5:1 against the selection), so a near-white surface would wash
    // selected text out. Those are replaced with each theme's proper dark
    // selection surface (Nord nord3 #4c566a, Catppuccin Surface2 #585b70).
    Builtin {
        name: "Dracula",
        appearance: Appearance::Dark,
        description: "vibrant dark theme — the Dracula palette",
        pair: None,
        signature: false,
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
        fg: c(0xc0, 0xca, 0xf5),
        bg: c(0x1a, 0x1b, 0x26),
        // Accent blue, not the fg lavender — a fg-coloured full-cell block reads as a
        // pale, heavy slab, heavier than the glyphs. The blue cursor
        // reads as a cursor and stays distinct from text.
        cursor: c(0x7a, 0xa2, 0xf7),
        // Upstream's visual-mode colour #33467c (the #283457 terminal port was
        // 1.4:1 on the background).
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
            // bright-black / "dim" tier lifted #414868 -> #7a82a8: the stock comment
            // grey was only ~2.8:1 on this very dark navy (below WCAG AA); now ~4.5:1
            // so de-emphasised/secondary text stays legible while still clearly dimmer.
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
        fg: c(0xeb, 0xdb, 0xb2),
        bg: c(0x28, 0x28, 0x28),
        cursor: c(0xeb, 0xdb, 0xb2),
        selection: c(0x66, 0x5c, 0x54),
        ansi: [
            // Upstream black is bg0 itself (invisible text); Gruvbox bg1.
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
        fg: c(0x83, 0x94, 0x96),
        bg: c(0x00, 0x2b, 0x36),
        cursor: c(0x83, 0x94, 0x96),
        // Upstream selection is base02 #073642, 1.15:1 on base03 (and equal to
        // color0); a base03-toward-base01 step at 1.5:1 that keeps base0 text
        // above 3:1 on it.
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
        fg: c(0xab, 0xb2, 0xbf),
        bg: c(0x21, 0x25, 0x2b),
        cursor: c(0xab, 0xb2, 0xbf),
        // One Dark's own editor selection #3e4451 (the terminal port's #323844
        // was 1.3:1 on the background).
        selection: c(0x3e, 0x44, 0x51),
        ansi: [
            // The port's black is the background itself, so `\e[30m` text was
            // invisible; One Dark's cursor-line grey #2c313a stays a near-black.
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
    // ── Light schemes ─────────────────────────────────────────────────────────
    // Light counterparts so the OS-appearance bridge (`aterm-gui` feeds the live
    // `winit` light/dark theme into the engine) has a real light scheme to switch
    // to. Same provenance as the dark set (github.com/mbadolato/iTerm2-Color-Schemes,
    // ghostty format). NOTE: several upstream light files set `selection` to the dark
    // *foreground* (they rely on a theme-supplied selection foreground, which aterm
    // themes do not carry — selected text keeps its own colour and the renderer only
    // floors it to 4.5:1). Those would render selected text dark-on-dark, readable
    // only through that floor, so each is replaced with that theme's proper LIGHT
    // selection surface — the mirror of the Nord/Catppuccin dark-selection fix above.
    // The `light_builtins_are_light_and_selection_legible` test guards every substitution.
    // aterm's own light look, the light half of Default: the same neutral
    // grey ink and green cursor family. On a light ground the bright tier is
    // the deeper one: brights 9-14 keep their own OkLCh hue and chroma one
    // shared step (L -0.06) below their normals, so each is at least 0.05
    // OkLab from its normal (ls shows both) and 6:1 or more on the
    // background; color15 is a dark slate between color8 and the ink, 7.9:1.
    // A light-mode OS gets this with no config.
    Builtin {
        name: "Default Light",
        appearance: Appearance::Light,
        description: "aterm's own light look, the light half of Default",
        pair: Some("Default"),
        signature: false,
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
        // base01, Solarized's emphasised-content tone: upstream's base00
        // #657b83 is 4.1:1 on base3, below the 4.5:1 body-text floor.
        fg: c(0x58, 0x6e, 0x75),
        bg: c(0xfd, 0xf6, 0xe3),
        cursor: c(0x58, 0x6e, 0x75),
        // Upstream selection is base2 #eee8d5, 1.14:1 on base3 (a selection
        // nobody can see); a base3-toward-base1 step at 1.5:1.
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
            // Upstream bright white is base3, the background itself; base2.
            c(0xee, 0xe8, 0xd5),
        ],
    },
    Builtin {
        name: "Gruvbox Light",
        appearance: Appearance::Light,
        description: "retro-groove light theme, warm earthy colours",
        pair: Some("Gruvbox Dark"),
        signature: false,
        fg: c(0x3c, 0x38, 0x36),
        bg: c(0xfb, 0xf1, 0xc7),
        cursor: c(0x3c, 0x38, 0x36),
        // Upstream selection is the dark fg #3c3836 (invisible as a bg highlight);
        // replaced with Gruvbox light3 #bdae93 — the mirror of dark3 #665c54 used by
        // Gruvbox Dark above.
        selection: c(0xbd, 0xae, 0x93),
        ansi: [
            // Upstream black is bg0 itself (invisible text); Gruvbox light1.
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
        fg: c(0x4c, 0x4f, 0x69),
        bg: c(0xef, 0xf1, 0xf5),
        // Rosewater, deepened (upstream #dc8a78 is 2.3:1 on the background, a
        // cursor that vanishes): upstream's own OkLCh hue and chroma at
        // lightness 0.61, 3.5:1.
        cursor: c(0xb9, 0x6a, 0x59),
        // Upstream selection is the Rosewater accent; replaced with Latte Surface2
        // #acb0be — the mirror of the Mocha Surface2 #585b70 substitution above.
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
        fg: c(0x1f, 0x23, 0x28),
        bg: c(0xff, 0xff, 0xff),
        cursor: c(0x09, 0x69, 0xda), // GitHub accent blue — upstream value, highly legible
        // Upstream selection is the dark fg #1f2328 (invisible as a bg highlight);
        // replaced with Primer blue-2 #80ccff (blue-1 #b6e3ff was 1.36:1 on white).
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

/// Look up a built-in scheme by name (case-insensitive). `"Default"` resolves to
/// [`ColorScheme::default`]; every other name is searched in [`struct@BUILTINS`].
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

/// All built-in scheme names (for `--list-themes` and config diagnostics), with
/// `"Default"` first.
#[must_use]
// Skip: iterator/collect absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn builtin_names() -> Vec<&'static str> {
    // `.min(64)` before the `+ 1` gives the Trust gate a literal upper bound on
    // the reservation (BUILTINS has far fewer than 64 entries, so the clamp is
    // never reached and the reserved capacity is unchanged in practice).
    let mut v = Vec::with_capacity(BUILTINS.len().min(64) + 1);
    v.push(DEFAULT_THEME.name);
    v.extend(BUILTINS.iter().map(|b| b.name));
    v
}

/// The identity half of a built-in theme: what listings, the picker and the
/// split resolver need without materializing its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinTheme {
    /// Canonical spelling (what `theme = "…"` and listings show).
    pub name: &'static str,
    /// The appearance the theme is drawn for (always the authored one).
    pub appearance: Appearance,
    /// One line for listings and the picker.
    pub description: &'static str,
    /// The same family's built-in for the other appearance, if aterm ships one.
    /// Pairs are symmetric and always cross appearances.
    pub pair: Option<&'static str>,
    /// One of aterm's own signature themes (listed right after Default).
    pub signature: bool,
}

/// `Default` lives in [`ColorScheme::default`], not [`struct@BUILTINS`]; this is
/// its identity row.
const DEFAULT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Default",
    appearance: Appearance::Dark,
    description: "aterm's historical dark look: pale grey text and a green cursor",
    pair: Some("Default Light"),
    signature: false,
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
// Skip: iterator/collect absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn builtin_theme_list() -> Vec<BuiltinTheme> {
    // `.min(64)` before the `+ 1` gives the Trust gate a literal upper bound on
    // the reservation (see `builtin_names`).
    let mut v = Vec::with_capacity(BUILTINS.len().min(64) + 1);
    v.push(DEFAULT_THEME);
    v.extend(BUILTINS.iter().map(Builtin::identity));
    v
}

/// The built-in paired with `name` for the other appearance (canonical
/// spelling), or `None` when `name` is not a built-in or has no pair.
#[must_use]
pub fn builtin_pair(name: &str) -> Option<&'static str> {
    builtin_theme(name)?.pair
}

/// All built-in schemes as `(name, description)` pairs, `"Default"` first — the
/// data behind `aterm list-themes`. Reads [`builtin_theme_list`] so the set
/// and its descriptions can't drift from the registry.
#[must_use]
pub fn builtin_themes() -> Vec<(&'static str, &'static str)> {
    builtin_theme_list()
        .into_iter()
        // `.take(64)` is identity — the list holds `BUILTINS.len() + 1`
        // entries (about a dozen) — but it hands the `collect` a literal
        // element bound so the Trust gate can prove the allocation bounded.
        .take(64)
        .map(|theme| (theme.name, theme.description))
        .collect()
}

/// Error from parsing a disk theme definition ([`parse_scheme_str`]).
/// Stringly-typed line context so a bad user theme file produces an actionable
/// diagnostic rather than a silent fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    /// A line was syntactically malformed (no `=`, bad colour, unknown or
    /// duplicate key, over-long text), or a required key was missing.
    Parse {
        /// 1-based line number of the offending line (`0` for a whole-file problem
        /// such as a missing required key).
        line: usize,
        /// Short human-readable reason for the failure.
        reason: String,
        /// The key the failure is about, canonically spelled (`foreground`,
        /// `color3`), or as written for an unknown key; `None` for a line
        /// with no key.
        key: Option<String>,
        /// The other terminal's theme format the file looks like, when it looks
        /// like one ([`sniff_foreign_format`]); such a file is converted with
        /// `aterm theme import`, never read natively.
        foreign_format: Option<ForeignFormat>,
    },
}

impl ThemeError {
    /// A [`ThemeError::Parse`] at `line` about `key` with no foreign-format
    /// hint.
    fn at(line: usize, key: Option<String>, reason: String) -> Self {
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
        // Trust gate: `write_str` instead of `write!` — runtime-argument
        // `format_args!` cannot be lowered natively. Byte-identical: nested
        // `{…}` always formats with default options, exactly like
        // `to_string`/`debug_string`.
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

/// Another terminal's theme file format, recognised by its shape so a parse
/// error can say what the file is and how to bring it in
/// (`aterm theme import`). The native parser never reads these formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignFormat {
    /// kitty: whitespace-separated `key value` lines (`foreground #c0caf5`).
    Kitty,
    /// Ghostty: hyphenated keys (`cursor-color`) or `palette = N=#hex`.
    Ghostty,
    /// An XML property list (iTerm2 `.itermcolors`, Terminal.app).
    Plist,
    /// TOML (Alacritty, WezTerm): `[table]` headers or quoted colour values.
    Toml,
    /// JSON (Windows Terminal, VS Code): a `{ … }` document.
    Json,
}

impl ForeignFormat {
    /// The user-facing name of the format.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ForeignFormat::Kitty => "kitty",
            ForeignFormat::Ghostty => "Ghostty",
            ForeignFormat::Plist => "property-list (iTerm2 / Terminal.app)",
            ForeignFormat::Toml => "TOML (Alacritty / WezTerm)",
            ForeignFormat::Json => "JSON (Windows Terminal)",
        }
    }
}

/// Guess which other terminal's theme format `text` is written in, or `None`
/// when it does not look like any of them. Pure and bounded by the text: it
/// reads each line once and stops at the first decisive one. Container shapes
/// (plist, JSON) are decided by the document's opening; line shapes (TOML,
/// kitty, Ghostty) by the first line that only that format writes. aterm's own
/// format (`key = value`, unquoted values, underscore keys) matches none.
#[must_use]
pub fn sniff_foreign_format(text: &str) -> Option<ForeignFormat> {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("<?xml")
        || head.starts_with("<!DOCTYPE plist")
        || head.starts_with("<plist")
    {
        return Some(ForeignFormat::Plist);
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
                return Some(ForeignFormat::Toml);
            }
            // kitty: `key value` with an identifier key and a colour value.
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
        // TOML: a quoted or array value (`foreground = "#c0caf5"`, `ansi = [ … ]`).
        if value.starts_with('"') || value.starts_with('\'') || value.starts_with('[') {
            return Some(ForeignFormat::Toml);
        }
        // Ghostty: `palette = N=#hex`, or a hyphenated key (`cursor-color`).
        if (key.eq_ignore_ascii_case("palette") && value.contains('=')) || key.contains('-') {
            return Some(ForeignFormat::Ghostty);
        }
    }
    None
}

/// Parse a theme colour: `#RRGGBB` (the `#` optional) or `#RGB` (the `#`
/// required, so a bare word such as `bad` or `fed` stays an error rather than
/// a colour), surrounding whitespace ignored. `#RGB` widens each digit to a
/// byte (`#abc` is `#aabbcc`), as the OSC colour specs do. `None` on malformed input. The one
/// hex-colour reader for theme files, so every reader of a theme colour
/// accepts the same spellings.
// Skip: radix parse absent std body; malformed input returns None.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let hashed = h.starts_with('#');
    let h = h.strip_prefix('#').unwrap_or(h);
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Every byte is an ASCII hex digit, so each total `get(..)` below is on a
    // char boundary and in bounds for its length arm; it carries no
    // slice-bounds obligation for the Trust gate.
    match h.len() {
        6 => Some(Rgb::new(
            u8::from_str_radix(h.get(0..2)?, 16).ok()?,
            u8::from_str_radix(h.get(2..4)?, 16).ok()?,
            u8::from_str_radix(h.get(4..6)?, 16).ok()?,
        )),
        3 if hashed => {
            // One digit `d` stands for the byte `dd`, i.e. `d * 17`.
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
/// These are shown in the theme picker and listings; a bound keeps one file
/// from flooding them.
pub const MAX_THEME_TEXT_CHARS: usize = 256;

/// A disk theme file as read by [`parse_theme_str`]: the colours plus the
/// identity text that is not part of a [`ColorScheme`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTheme {
    /// The colours, name and appearance.
    pub scheme: ColorScheme,
    /// The file's `description`, if any.
    pub description: Option<String>,
    /// The file's `author`, if any.
    pub author: Option<String>,
    /// Whether the file set `appearance` itself. When it did not,
    /// `scheme.appearance` is a `Dark` placeholder until admission
    /// ([`ThemeMeta::admit`]) writes the measured one.
    pub appearance_declared: bool,
}

/// Where a theme's [`Appearance`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceSource {
    /// The author said so (a built-in, or a file's `appearance` key).
    Declared,
    /// The file left it out; the catalog measured its background.
    Measured,
}

/// Lowest foreground-on-background contrast a theme is admitted without a
/// warning ([`ThemeWarning::LowTextContrast`]).
pub const MIN_TEXT_CONTRAST: f64 = 3.0;
/// Lowest selection-on-background contrast a theme is admitted without a
/// warning ([`ThemeWarning::FaintSelection`]).
pub const MIN_SELECTION_CONTRAST: f64 = 1.3;
/// Lowest cursor-on-background contrast a theme is admitted without a warning
/// ([`ThemeWarning::LowCursorContrast`]).
pub const MIN_CURSOR_CONTRAST: f64 = 3.0;

/// A problem a theme file is admitted with: it still loads, but the listing
/// and picker say what will be hard to read. Ratios are WCAG contrast in
/// hundredths (`250` is 2.50:1) so the type stays `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeWarning {
    /// Foreground on background below [`MIN_TEXT_CONTRAST`].
    LowTextContrast {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// The selection band on background below [`MIN_SELECTION_CONTRAST`]: a
    /// selection is hard to see.
    FaintSelection {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// An ANSI slot is exactly the background, so text in that colour is
    /// invisible.
    AnsiMatchesBackground {
        /// The slot, `0..16`.
        slot: u8,
    },
    /// An explicit cursor colour on background below [`MIN_CURSOR_CONTRAST`].
    LowCursorContrast {
        /// The measured ratio, in hundredths.
        hundredths: u32,
    },
    /// The file declares one appearance and its background measures as the
    /// other.
    AppearanceMismatch {
        /// What the file says.
        declared: Appearance,
        /// What its background measures as.
        measured: Appearance,
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
            Self::AppearanceMismatch { declared, measured } => write!(
                f,
                "declares appearance = {} but its background is {}",
                appearance_word(declared),
                appearance_word(measured)
            ),
        }
    }
}

/// The colour-identity half of a theme (DTP §3.1): what a listing, the picker
/// and the split resolver show and pair by, separate from the colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeMeta {
    /// What a person sees: a file's `name =`, else its id (the file stem,
    /// which stays what config writes); a built-in's canonical name.
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
}

impl ThemeMeta {
    /// A built-in's identity. Built-ins are curated, so they carry no
    /// admission warnings.
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
        }
    }

    /// Admit a parsed theme file whose id is `id` (its file stem).
    /// `measured` is the appearance its background measures as, by the
    /// caller's one dark/light classifier (this crate does no luminance
    /// policy). A file that leaves `appearance` out takes `measured`, written
    /// into the scheme too, instead of a silent `Dark`.
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
        Self {
            display_name,
            description: parsed.description.clone(),
            author: parsed.author.clone(),
            appearance: scheme.appearance,
            appearance_source,
            pair: None,
            warnings: admission_warnings(scheme, measured),
        }
    }
}

/// Every [`ThemeWarning`] `scheme` earns, in a fixed order: text, selection,
/// ANSI slots (ascending), cursor, appearance. The contrast warnings are
/// exactly the [`contrast_report`] rows below their minimum.
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
    if scheme.appearance != measured {
        warnings.push(ThemeWarning::AppearanceMismatch {
            declared: scheme.appearance,
            measured,
        });
    }
    warnings
}

/// Two colours of a theme whose contrast [`contrast_report`] measures: the
/// first is drawn on the second.
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
}

impl ContrastPair {
    /// The two colours as theme keys: `(drawn, on)`, an ANSI slot as
    /// `color<N>`. The selection band is `selection` even when derived.
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
    /// The admission bar, in hundredths, when a theme is warned below it
    /// ([`admission_warnings`]); `None` for a pair measured for information.
    pub minimum: Option<u32>,
    /// Whether the ratio is below the bar, by the exact comparison admission
    /// makes (`None` with no bar).
    below: Option<bool>,
}

impl ContrastRow {
    /// Whether this pair is below its admission bar (never, with no bar).
    #[must_use]
    pub fn below_minimum(&self) -> bool {
        self.below.unwrap_or(false)
    }
}

/// Every contrast a theme's legibility rests on, in a fixed order: text on
/// background, each ANSI slot on background, the selection band on
/// background, text on the selection (and `selection_foreground` on it, when
/// set), then the cursor on background. A pair has a minimum when admission
/// warns below it: text and an explicit cursor at [`MIN_TEXT_CONTRAST`] and
/// [`MIN_CURSOR_CONTRAST`], the band at [`MIN_SELECTION_CONTRAST`]. A cursor
/// the theme leaves out follows the foreground, so it has no bar of its own.
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
    rows
}

/// A theme as the catalog holds it, under the id `theme = "<id>"` names: a
/// built-in, or a themes-folder file that loaded. What `aterm theme export`
/// writes and `aterm theme new --from` starts from.
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
            theme: ParsedTheme {
                scheme,
                description: Some(identity.description.to_string()),
                author: None,
                appearance_declared: true,
            },
            meta: ThemeMeta::of_builtin(identity),
        })
    }
}

/// One key of the theme format, by what it sets. Every spelling of a key
/// (`fg` and `foreground`, `palette3` and `color3`) is the same `ThemeKey`, so
/// setting it twice under two spellings is still a duplicate.
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
}

impl ThemeKey {
    /// This key's row of [`THEME_KEYS`]. Exhaustive, so a new key cannot
    /// compile without a row; the assertion under [`THEME_KEYS`] checks at
    /// compile time that each row sits where this says.
    #[must_use]
    pub const fn row(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Description => 1,
            Self::Author => 2,
            Self::Appearance => 3,
            Self::Foreground => 4,
            Self::Background => 5,
            Self::Cursor => 6,
            Self::Selection => 7,
            Self::SelectionForeground => 8,
            Self::Palette => 9,
        }
    }
}

/// What a theme key's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeValueType {
    /// A colour: `#RRGGBB` (the `#` optional) or `#RGB` ([`parse_hex`]).
    Colour,
    /// One line of text: at most [`MAX_THEME_TEXT_CHARS`] chars and no control
    /// characters; a ` #` or ` ;` in it starts an inline comment.
    Text,
    /// One of [`APPEARANCE_VALUES`], case-insensitive.
    Appearance,
}

impl ThemeValueType {
    /// The type's name in `aterm theme schema`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Colour => "colour",
            Self::Text => "text",
            Self::Appearance => "appearance",
        }
    }
}

/// The values the `appearance` key takes, as written (case-insensitive).
pub const APPEARANCE_VALUES: [&str; 2] = ["dark", "light"];

/// How many ANSI slots a theme sets (`color0` through `color15`).
pub const THEME_PALETTE_SLOTS: u8 = 16;

/// One row of [`THEME_KEYS`]: everything the parser and `aterm theme schema`
/// know about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeKeySpec {
    /// What the key sets.
    pub key: ThemeKey,
    /// The canonical spelling. For an indexed key it is the stem (`color`),
    /// written with the slot number after it (`color3`).
    pub name: &'static str,
    /// The other spellings the parser accepts (stems, for an indexed key).
    pub aliases: &'static [&'static str],
    /// The value's type.
    pub value: ThemeValueType,
    /// Whether a theme without this key is refused.
    pub required: bool,
    /// For an indexed key, how many slots it has (`0..indexed`); `0` for a
    /// plain key.
    pub indexed: u8,
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

/// THE table of theme keys: [`parse_theme_str`] reads keys by it, and
/// `aterm theme schema` and the `aterm theme new` template are printed from
/// it, so neither can describe a key the parser does not read the same way.
/// The order is the order a theme file is written in.
pub const THEME_KEYS: [ThemeKeySpec; 10] = [
    ThemeKeySpec {
        key: ThemeKey::Name,
        name: "name",
        aliases: &[],
        value: ThemeValueType::Text,
        required: false,
        indexed: 0,
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
        when_omitted: "measured from the background",
        description: "Whether the theme is drawn for a dark or a light background; a dark:/light: theme split picks by it.",
        example: "dark",
    },
    ThemeKeySpec {
        key: ThemeKey::Foreground,
        name: "foreground",
        aliases: &["fg"],
        value: ThemeValueType::Colour,
        required: true,
        indexed: 0,
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
        when_omitted: "the slot's xterm default",
        description: "ANSI slot N: 0-7 black, red, green, yellow, blue, magenta, cyan, white; 8-15 their bright forms.",
        example: "#f7768e",
    },
];

/// The [`THEME_KEYS`] row for `key`.
#[must_use]
pub fn theme_key_spec(key: ThemeKey) -> &'static ThemeKeySpec {
    &THEME_KEYS[key.row()]
}

// Each row of the table is the one its key's `row()` names, so the rows are
// distinct keys and `theme_key_spec` returns the key it was asked for.
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

/// Read a key as written (any case, any spelling) by [`THEME_KEYS`]: its row's
/// position in the table and, for an indexed key, the slot. `None` for a key
/// the format does not have.
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
            // Digits only: `color+1` and `color 1` are not slot 1.
            if slot.is_empty() || !slot.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let slot = slot.parse::<u8>().ok().filter(|n| *n < spec.indexed)?;
            Some((row, Some(slot)))
        })
    })
}

/// The line each key of a parsed theme was set on, as [`parse_theme_located`]
/// returns it, so a report about a key can point at its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeKeyLines {
    /// Per [`THEME_KEYS`] row (plain keys only).
    plain: [Option<usize>; THEME_KEYS.len()],
    /// Per ANSI slot.
    palette: [Option<usize>; THEME_PALETTE_SLOTS as usize],
}

impl ThemeKeyLines {
    /// The 1-based line `key` (slot `index`, for the palette) was set on, or
    /// `None` when the file leaves it out.
    #[must_use]
    pub fn line(&self, key: ThemeKey, index: Option<u8>) -> Option<usize> {
        if key == ThemeKey::Palette {
            return *self.palette.get(usize::from(index?))?;
        }
        *self.plain.get(key.row())?
    }
}

/// Parse a minimal disk theme definition into a [`ColorScheme`]: exactly
/// [`parse_theme_str`] without the identity text.
///
/// # Errors
/// As [`parse_theme_str`].
pub fn parse_scheme_str(text: &str) -> Result<ColorScheme, ThemeError> {
    parse_theme_str(text).map(|parsed| parsed.scheme)
}

/// Parse a disk theme definition.
///
/// FORMAT — a deliberately tiny, dependency-free `key = value` line format (so this
/// bottom-of-graph crate needs no TOML/serde at runtime). It is aterm's own
/// format: Ghostty theme files (`palette = N=#…`, `cursor-color`), kitty's
/// `key value` files, Alacritty's TOML, iTerm2 plists and Windows Terminal JSON
/// are NOT accepted as-is. Parsing one fails, and the error names the format it
/// looks like (the `foreign_format` of [`ThemeError::Parse`]) so the user can convert
/// it with `aterm theme import`.
///
/// ```text
/// # comments start with '#' (or ';'); blank lines ignored
/// name        = My Theme        # optional display name
/// description = Deep navy       # optional
/// author      = Jane Doe        # optional
/// appearance  = dark            # dark | light; omitted => measured from background
/// foreground  = #c0caf5         # alias fg
/// background  = #1a1b26         # alias bg
/// cursor_color = #7aa2f7        # alias cursor; omitted => follows foreground
/// selection_color = #283457     # alias selection; omitted => bg 22% toward fg
/// selection_foreground = #c0caf5  # optional; omitted => selected text keeps its colour
/// # the 16 ANSI slots, by 0-based index (0-7 normal, 8-15 bright):
/// color0 = #15161e
/// color1 = #f7768e
/// # … through color15. Aliases: `palette0`/`ansi0` also accepted.
/// ```
///
/// The keys, their spellings, types and which are required are
/// [`THEME_KEYS`]; the parser reads keys by that table.
///
/// SEMANTICS: `foreground` and `background` are REQUIRED (a theme with no fg/bg is
/// rejected). Any unspecified `colorN` defaults to that slot's
/// [`ColorPalette::default_color`], so a partial palette is still well-formed.
/// Colours are `#RRGGBB` or `#RGB` ([`parse_hex`]). `name`, `description` and
/// `author` are at most [`MAX_THEME_TEXT_CHARS`] chars with no control
/// characters. A leading UTF-8 byte-order mark is ignored. Keys are
/// case-insensitive. An unknown key, a malformed colour or
/// text, or a key set twice (under any of its spellings, e.g. `foreground` then
/// `fg`, or `color1` then `palette1`) is a [`ThemeError::Parse`] with the 1-based
/// line number.
///
/// # Errors
/// Returns [`ThemeError::Parse`] for a malformed line, a bad colour value, an
/// unknown or duplicate key, over-long text, or a missing required
/// `foreground`/`background`.
// Skip: the parse closures drive absent std iterator/parse bodies; every malformed input is rejected (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_theme_str(text: &str) -> Result<ParsedTheme, ThemeError> {
    parse_theme_located(text).map(|(parsed, _)| parsed)
}

/// [`parse_theme_str`], and the line each key was set on (for a report that
/// points at a key, such as `aterm theme check`).
///
/// # Errors
/// As [`parse_theme_str`].
// Skip: the parse closures drive absent std iterator/parse bodies; every malformed input is rejected (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_theme_located(text: &str) -> Result<(ParsedTheme, ThemeKeyLines), ThemeError> {
    // A UTF-8 byte-order mark (Windows editors write one) is no part of the
    // first key; the importer strips it too, so the two readers agree. Line
    // numbers are unchanged: the mark sits inside line 1.
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

/// The line parser behind [`parse_theme_located`], which adds the
/// foreign-format hint to its error. Keys are read by [`THEME_KEYS`].
// Skip: the parse closures drive absent std iterator/parse bodies; every malformed input is rejected (fail-closed).
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
    // Start from the engine defaults so an omitted `colorN` is render-identical to
    // an unthemed slot.
    let mut ansi = ColorScheme::default().ansi;
    // The line each key was first set on, so a second setting (under any
    // spelling) is caught and the error can point back at the first.
    let mut lines = ThemeKeyLines {
        plain: [None; THEME_KEYS.len()],
        palette: [None; THEME_PALETTE_SLOTS as usize],
    };

    for (idx, raw) in text.lines().enumerate() {
        // Trust gate: `saturating_add(1)` is identical to `+ 1` here — `idx`
        // counts `lines()` items, and a `&str` holds at most `isize::MAX`
        // bytes, so `idx < usize::MAX` and the saturation branch never fires.
        let lineno = idx.saturating_add(1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Strip an inline trailing comment (`key = #fff  # note`) — but NOT a '#'
        // that begins a hex value. We split on the FIRST '=' to isolate the value,
        // then trim a comment that starts with whitespace-' #'/';'.
        // Trust gate (this fn): parse errors are concatenated instead of
        // `format!` — runtime-argument `format_args!` cannot be lowered
        // natively. `debug_string` is byte-identical to `{…:?}`.
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
                    // The table types no other key as a colour.
                    _ => {}
                }
            }
            ThemeValueType::Text => {
                let text = bounded_text(lineno, spec.name, &value)?;
                match spec.key {
                    ThemeKey::Name => name = text,
                    ThemeKey::Description => description = Some(text),
                    ThemeKey::Author => author = Some(text),
                    // The table types no other key as text.
                    _ => {}
                }
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

    // The table says which keys a theme cannot leave out.
    for (row, spec) in THEME_KEYS.iter().enumerate() {
        if spec.required && spec.indexed == 0 && lines.plain[row].is_none() {
            return Err(missing_key(spec.name));
        }
    }
    let (Some(foreground), Some(background)) = (foreground, background) else {
        // Unreachable while the table marks both required (the
        // `theme_key_table_drives_the_parser` test); refused, not defaulted.
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
        },
        description,
        author,
        appearance_declared: appearance.is_some(),
    };
    Ok((parsed, lines))
}

/// A whole-file [`ThemeError::Parse`] for a required key the file leaves out.
fn missing_key(name: &str) -> ThemeError {
    let mut reason = String::from("missing required key '");
    reason.push_str(name);
    reason.push('\'');
    ThemeError::at(0, Some(name.to_string()), reason)
}

/// A `name` / `description` / `author` value, refused when it is longer than
/// [`MAX_THEME_TEXT_CHARS`] or carries a control character.
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

/// A [`ThemeError::Parse`] at `line` (about `key`, when there is one) whose
/// reason is `prefix` then `detail` rendered as `{detail:?}` (via
/// `debug_string`, see `parse_theme_lines`).
fn parse_error(line: usize, key: Option<&str>, prefix: &str, detail: &str) -> ThemeError {
    let mut reason = String::from(prefix);
    reason.push_str(&crate::trust_fmt::debug_string(detail));
    ThemeError::at(line, key.map(str::to_string), reason)
}

/// Strip a trailing ` #…` / ` ;…` inline comment from a value, but never a leading
/// `#` that is part of a hex colour. Only whitespace-preceded `#`/`;` (or one at the
/// very start, which can't be a value) is treated as a comment.
fn strip_inline_comment(value: &str) -> &str {
    let mut prev_ws = true; // start is a boundary
    for (i, b) in value.bytes().enumerate() {
        if (b == b'#' || b == b';') && prev_ws {
            // A whitespace-preceded `#`/`;` is a comment UNLESS it is the leading
            // token (i.e. everything before it is blank) — that case is a hex value
            // like ` #fff`, which must be kept. `i` is a byte offset from
            // `bytes().enumerate()`, always <= len at an ASCII ('#'/';') boundary,
            // so the total `get(..i)` yields `Some` — identical to `value[..i]`
            // without a slice-bounds obligation.
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

/// The user theme directory: `themes` inside the folder that holds
/// `aterm.toml` ([`crate::dirs::aterm_config_dir`]), so it follows the same
/// XDG / `%APPDATA%` / `HOME` rule as the config on every platform. `None` when
/// no config directory can be resolved. Public so a packaging / `--list-themes`
/// caller can tell users exactly where to drop theme packs.
#[must_use]
pub fn user_theme_dir() -> Option<std::path::PathBuf> {
    crate::dirs::aterm_config_dir().map(|dir| dir.join("themes"))
}

/// Validate the literal grammar shared by direct loading and the GUI's bounded
/// theme catalog. Theme names are file stems, never paths or OS-split syntax.
/// Keeping this at the lowest layer makes every loader traversal-safe.
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

/// One comma segment of a `theme` value, classified by the single rule every
/// reader of the selector grammar shares ([`ThemeSelector::parse`] and
/// [`validate_user_theme_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorSegment<'a> {
    /// `dark:<name>` / `light:<name>`: the key is case- and space-insensitive,
    /// and the name is trimmed (it may be empty).
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

/// A parsed `theme` value: one name for every appearance, or a
/// `dark:<name>,light:<name>` split. The one grammar the runtime resolver,
/// config validation and the Settings disclosure all read, so they cannot
/// disagree about what a value means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSelector {
    /// A plain name, trimmed, applied to both appearances. Empty only when the
    /// whole value was blank (reported as [`SelectorIssue::EmptyName`]).
    Single(String),
    /// A split: any segment's key is `dark` or `light`. A side is `None` when
    /// the value does not name it (or names it unusably).
    Split {
        /// The name for a dark OS appearance.
        dark: Option<String>,
        /// The name for a light OS appearance.
        light: Option<String>,
    },
}

/// Why part of a `theme` value is not used as written. The runtime still draws
/// what it can read ([`ThemeSelector::name_for`]); validation reports these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorIssue {
    /// A side appears more than once; the first one is used.
    DuplicateSide(Appearance),
    /// A `<mode>:<name>` segment in a split whose mode is not dark or light
    /// (lower-cased).
    UnknownMode(String),
    /// A side with no name (`dark:`), or a blank value (`None`).
    EmptyName(Option<Appearance>),
    /// A segment in a split with no `<mode>:` at all.
    JunkSegment(String),
}

fn appearance_word(appearance: Appearance) -> &'static str {
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
    /// Parse a `theme` value. A value is a split when any comma segment's key
    /// is `dark` or `light`; otherwise the whole trimmed value is one name
    /// (which may itself contain `:`). In a split the first usable segment for
    /// each side wins, and every segment that is not used is one issue.
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

    /// The name this selector draws for `appearance`: a plain name for both;
    /// a split's own side; for a side the split leaves out, the named side's
    /// built-in pair ([`builtin_pair`], so `dark:Gruvbox Dark` draws Gruvbox
    /// Light on a light OS), else the named side itself. `None` means no name
    /// at all (the built-in Default).
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

    /// The names the value writes, each with the appearance it is written for
    /// (`None` for a plain name that serves both), in dark-then-light order.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_cover_every_name_default_first_and_resolve() {
        let names = builtin_names();
        let themes = builtin_themes();
        // Same set, same order, as builtin_names — Default first.
        assert_eq!(
            names,
            themes.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "builtin_themes must mirror builtin_names"
        );
        assert_eq!(themes.first().map(|(n, _)| *n), Some("Default"));
        // Every name resolves to a scheme whose appearance is the identity
        // row's, and every description is non-empty and its own.
        let mut descriptions = std::collections::BTreeSet::new();
        for (name, desc) in &themes {
            assert!(!desc.is_empty(), "{name} has an empty description");
            assert!(descriptions.insert(*desc), "{name} reuses a description");
            let scheme = builtin(name).expect("a listed built-in resolves");
            let identity = builtin_theme(name).expect("a listed built-in has an identity");
            assert_eq!(identity.name, *name);
            assert_eq!(scheme.appearance, identity.appearance, "{name}");
        }
        assert_eq!(builtin_theme("no such theme"), None);
    }

    /// Law: a built-in pair is symmetric, crosses appearances and names a
    /// built-in; the families aterm ships both halves of are linked.
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
        // Lookup is case-insensitive and answers canonically; an unpaired or
        // unknown name has none.
        assert_eq!(builtin_pair("gruvbox dark"), Some("Gruvbox Light"));
        assert_eq!(builtin_pair("Nord"), None);
        assert_eq!(builtin_pair("Studio Night"), None);
    }

    fn admitted(text: &str, id: &str, measured: Appearance) -> (ParsedTheme, ThemeMeta) {
        let mut parsed = parse_theme_str(text).expect("theme parses");
        let meta = ThemeMeta::admit(&mut parsed, id, measured);
        (parsed, meta)
    }

    /// A file's `name =` is its display name and its stem stays the id; an
    /// omitted `appearance` takes the measured one, a declared one is kept.
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

    /// Each admission warning fires on its defect and not on a clean theme.
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
        };
        // Negative control: the clean base earns nothing.
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
        // An unset cursor follows the foreground: no separate cursor warning.
        let mut s = base.clone();
        s.cursor = None;
        assert_eq!(kinds(&s), vec![]);
        // The ratio is carried in hundredths and printed as a ratio.
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

    /// A built-in's meta is its identity row, with no warnings.
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

    /// The selector grammar, as a table: what the runtime draws for each
    /// appearance and which issue kinds validation reports.
    #[test]
    fn theme_selector_table() {
        use Appearance::{Dark, Light};
        use SelectorIssue as I;
        /// `(value, dark draws, light draws, issue kinds)`.
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
            // One-sided: the omitted side draws the named side's built-in
            // pair, else the named side itself.
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
            // Duplicates: the first wins, the second is an issue.
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
            // Side-carrying kinds name the side they are about.
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

    /// Law: a value is a split exactly when some segment classifies as a side,
    /// and a name the validator admits is always a plain single selector that
    /// round-trips as itself (so a catalog id can be written back verbatim).
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
        // Negative control: a name the validator refuses as split syntax
        // does not parse as a single name.
        assert!(validate_user_theme_name("dark:Nord").is_err());
        assert!(matches!(
            ThemeSelector::parse("dark:Nord").0,
            ThemeSelector::Split { .. }
        ));
    }

    /// The Default scheme's ANSI slots equal `ColorPalette::default_color` (one
    /// source) and its `to_color_palette()` resolves every index to the same RGB.
    #[test]
    fn default_scheme_matches_palette_defaults() {
        let s = ColorScheme::default();
        let p = s.to_color_palette();
        for i in 0u16..=255 {
            let idx = i as u8;
            assert_eq!(p.get(idx), ColorPalette::default_color(idx), "index {idx}");
        }
        // The blue lift is reflected (index 4 is the readable blue, not pure blue).
        assert_eq!(s.ansi[4], Rgb::new(59, 142, 234));
        assert_ne!(s.ansi[4], Rgb::new(0, 0, 238));
    }

    /// Default chrome projects to the historical renderer Theme values.
    #[test]
    fn default_theme_parts_are_historical() {
        let tp = ColorScheme::default().to_theme_parts();
        assert_eq!(tp.fg, 0x00D0_D0D0);
        assert_eq!(tp.bg, 0x0011_1318);
        assert_eq!(tp.cursor, 0x0050_FA7B);
        // Selection darkened from #264F78 → #33415E so selected coloured text stays
        // readable (LLM-judge finding V3); kept in sync with the renderer Theme.
        assert_eq!(tp.selection, 0x0033_415E);
    }

    /// `builtin("default")` round-trips; an unknown name is `None`.
    #[test]
    fn builtin_lookup() {
        assert_eq!(builtin("default"), Some(ColorScheme::default()));
        assert_eq!(builtin("DEFAULT"), Some(ColorScheme::default()));
        assert!(builtin("no-such-theme-xyz").is_none());
        assert_eq!(builtin_names()[0], "Default");
    }

    /// Every built-in is internally well-formed (non-empty name, distinct from the
    /// reserved "Default" sentinel) and round-trips through `to_scheme`.
    #[test]
    fn builtins_well_formed() {
        for b in BUILTINS {
            assert!(!b.name.is_empty());
            assert!(
                !b.name.eq_ignore_ascii_case("default"),
                "{} shadows Default",
                b.name
            );
            let s = b.to_scheme();
            assert_eq!(s.name, b.name);
            assert_eq!(builtin(b.name).as_ref(), Some(&s));
        }
    }

    /// Golden spot-checks of actual palette VALUES (not just structure), so a
    /// corrupted/typo'd source constant can't pass vacuously — including the
    /// deliberate Nord/Catppuccin dark-selection substitutions. Verified against
    /// github.com/mbadolato/iTerm2-Color-Schemes.
    #[test]
    fn builtin_palette_values_are_canonical() {
        let g = |name: &str| builtin(name).expect("builtin exists");
        // Dracula: bg, fg, ANSI red, ANSI bright-blue.
        assert_eq!(g("Dracula").background, Rgb::new(0x28, 0x2a, 0x36));
        assert_eq!(g("Dracula").foreground, Rgb::new(0xf8, 0xf8, 0xf2));
        assert_eq!(g("Dracula").ansi[1], Rgb::new(0xff, 0x55, 0x55));
        assert_eq!(g("Dracula").ansi[12], Rgb::new(0xd6, 0xac, 0xff));
        // Nord: bg + the substituted dark selection (#4c566a, not the white #eceff4).
        assert_eq!(g("Nord").background, Rgb::new(0x2e, 0x34, 0x40));
        assert_eq!(g("Nord").selection, Some(Rgb::new(0x4c, 0x56, 0x6a)));
        // Catppuccin Mocha: the substituted dark selection (#585b70).
        assert_eq!(
            g("Catppuccin Mocha").selection,
            Some(Rgb::new(0x58, 0x5b, 0x70))
        );
        // Tokyo Night + Solarized Dark + Gruvbox + One Dark anchors.
        assert_eq!(g("Tokyo Night").background, Rgb::new(0x1a, 0x1b, 0x26));
        assert_eq!(g("Solarized Dark").ansi[1], Rgb::new(0xdc, 0x32, 0x2f));
        assert_eq!(g("Solarized Dark").background, Rgb::new(0x00, 0x2b, 0x36));
        assert_eq!(g("Gruvbox Dark").foreground, Rgb::new(0xeb, 0xdb, 0xb2));
        assert_eq!(g("One Dark").ansi[4], Rgb::new(0x61, 0xaf, 0xef));
        // Light schemes: a chrome anchor + the deliberate light-selection
        // substitutions (so a corrupted constant can't pass vacuously).
        assert_eq!(g("Solarized Light").background, Rgb::new(0xfd, 0xf6, 0xe3));
        assert_eq!(
            g("Solarized Light").selection,
            Some(Rgb::new(0xcf, 0xc9, 0xb7))
        );
        assert_eq!(g("Gruvbox Light").background, Rgb::new(0xfb, 0xf1, 0xc7));
        assert_eq!(
            g("Gruvbox Light").selection,
            Some(Rgb::new(0xbd, 0xae, 0x93))
        );
        assert_eq!(g("Catppuccin Latte").background, Rgb::new(0xef, 0xf1, 0xf5));
        assert_eq!(
            g("Catppuccin Latte").selection,
            Some(Rgb::new(0xac, 0xb0, 0xbe))
        );
        assert_eq!(g("GitHub Light").background, Rgb::new(0xff, 0xff, 0xff));
        assert_eq!(
            g("GitHub Light").selection,
            Some(Rgb::new(0x80, 0xcc, 0xff))
        );
    }

    /// Law: every built-in, Default included, is legible as shipped. Body
    /// text clears 4.5:1, the selection band reads as a band (1.5:1 on the
    /// background), the cursor clears 3:1, and no ANSI slot is the
    /// background (text in it would be invisible, and `minimum_contrast` is
    /// off by default, so nothing rescues it).
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
            // The admission check a user file gets finds nothing either.
            assert_eq!(
                admission_warnings(&s, s.appearance),
                [],
                "{name} would be admitted with warnings"
            );
        }
    }

    /// Law: aterm's signature themes clear their bars (text 7:1, every
    /// chromatic slot 4.5:1, hues 25 deg apart, distinct brights, color8
    /// between 3:1 and 4.5:1, a visible color0, legible whites, a 1.5:1
    /// band under 4.5:1 text, a chromatic accent cursor at 3:1, balanced
    /// chroma), and each pairs with a signature theme of the other
    /// appearance. Negative control: Default, which is not one, misses bars.
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

    /// Default first, then the signature themes, then the rest.
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

    /// A theme file that leaves `selection` out gets a band on its own
    /// background's side: a light theme a light band, a dark theme a dark
    /// one. Negative control: the fixed dark default it used to get sits on
    /// the light theme's text side.
    #[test]
    fn an_unset_selection_follows_the_theme() {
        let light = parse_scheme_str("foreground = #24292f\nbackground = #fafafa\n")
            .expect("a minimal light theme parses");
        assert_eq!(light.selection, None);
        let (fg, bg) = (light.foreground, light.background);
        let band = light.selection_band();
        // On the background's side of the theme, and visible on it.
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
        // A set selection wins.
        let set =
            parse_scheme_str("fg = #000000\nbg = #ffffff\nselection = #123456\n").expect("parses");
        assert_eq!(set.selection_band(), Rgb::new(0x12, 0x34, 0x56));
    }

    /// The blend's arithmetic: its end points and its rounding.
    #[test]
    fn derived_selection_is_the_rounded_blend() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        // 255 * 22 / 100 = 56.1 -> 56; 255 * 78 / 100 = 198.9 -> 199.
        assert_eq!(derived_selection(white, black), Rgb::new(56, 56, 56));
        assert_eq!(derived_selection(black, white), Rgb::new(199, 199, 199));
        let grey = Rgb::new(0x80, 0x80, 0x80);
        assert_eq!(derived_selection(grey, grey), grey);
    }

    /// Every built-in light scheme is actually light (`Appearance::Light` AND a
    /// background brighter than its foreground), and on EVERY built-in — dark or
    /// light — the default foreground stays legible OVER the selection surface
    /// (contrast ≥ 3.0:1). The built-ins carry no selection foreground — selected text
    /// keeps its own colour, which the renderer floors to 4.5:1 against the selection —
    /// so a scheme whose `selection` collapses onto the text colour would lean on that
    /// floor for every selected cell; this guards the light-theme dark-fg→light-surface substitutions.
    #[test]
    fn light_builtins_are_light_and_selection_legible() {
        // A quick perceptual luma (0-255) so "light" means what the eye sees.
        let luma =
            |c: Rgb| 0.299 * f64::from(c.r) + 0.587 * f64::from(c.g) + 0.114 * f64::from(c.b);
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
            // Selection legibility: the band the scheme paints must hold
            // ≥ 3.0:1 against the default foreground.
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

    /// A full disk theme string parses into the expected `ColorScheme`: chrome,
    /// appearance, name, and every ANSI slot (with both `colorN` and alias keys).
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
        // ANSI slots, including the two alias-keyed entries.
        assert_eq!(s.ansi[0], Rgb::new(0x15, 0x16, 0x1e));
        assert_eq!(s.ansi[1], Rgb::new(0xf7, 0x76, 0x8e));
        assert_eq!(s.ansi[2], Rgb::new(0x9e, 0xce, 0x6a)); // palette2
        assert_eq!(s.ansi[3], Rgb::new(0xe0, 0xaf, 0x68)); // ansi3
        assert_eq!(s.ansi[15], Rgb::new(0xc0, 0xca, 0xf5));
        // It projects through the existing sinks exactly like a built-in.
        assert_eq!(s.to_theme_parts().bg, 0x001a_1b26);
        assert_eq!(s.to_color_palette().get(1), Rgb::new(0xf7, 0x76, 0x8e));
    }

    /// A minimal theme (only the required fg/bg) is well-formed: missing ANSI slots
    /// fall back to the engine defaults, cursor/selection stay `None`.
    #[test]
    fn parse_scheme_str_minimal_defaults_unset_slots() {
        let s = parse_scheme_str("foreground = #ffffff\nbackground = #000000\n")
            .expect("minimal theme parses");
        assert_eq!(s.foreground, Rgb::new(0xff, 0xff, 0xff));
        assert_eq!(s.background, Rgb::new(0, 0, 0));
        assert_eq!(s.cursor, None);
        assert_eq!(s.selection, None);
        // Unspecified slots equal the engine defaults (render-identical to unthemed).
        for i in 0u8..16 {
            assert_eq!(
                s.ansi[i as usize],
                ColorPalette::default_color(i),
                "slot {i}"
            );
        }
        // `fg`/`bg` short aliases also work.
        let s2 = parse_scheme_str("fg = #abcdef\nbg = #123456\n").unwrap();
        assert_eq!(s2.foreground, Rgb::new(0xab, 0xcd, 0xef));
        assert_eq!(s2.background, Rgb::new(0x12, 0x34, 0x56));
    }

    /// Malformed inputs surface a precise `ThemeError::Parse` (line + reason);
    /// missing required keys are rejected.
    #[test]
    fn parse_scheme_str_rejects_malformed() {
        // No '=' on a content line.
        let e = parse_scheme_str("foreground = #fff000\nnonsense\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
        // Bad colour.
        let e = parse_scheme_str("foreground = #zzzzzz\nbackground = #000000\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 1, .. }), "{e:?}");
        // Unknown key.
        let e =
            parse_scheme_str("foreground = #fff000\nbackground=#000000\nwidth = 80\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 3, .. }), "{e:?}");
        // Out-of-range index.
        let e = parse_scheme_str("foreground=#ffffff\nbackground=#000000\ncolor16=#111111\n")
            .unwrap_err();
        assert!(matches!(e, ThemeError::Parse { .. }), "{e:?}");
        // Neither 3 nor 6 digits.
        let e = parse_scheme_str("fg=#ffff\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 1, .. }), "{e:?}");
        // Duplicate colour index.
        let e = parse_scheme_str(
            "foreground=#ffffff\nbackground=#000000\ncolor1=#111111\ncolor1=#222222\n",
        )
        .unwrap_err();
        assert!(
            matches!(&e, ThemeError::Parse { reason, .. } if reason.contains("duplicate")),
            "{e:?}"
        );
        // Missing required background.
        let e = parse_scheme_str("foreground = #ffffff\n").unwrap_err();
        assert!(
            matches!(&e, ThemeError::Parse { reason, .. } if reason.contains("background")),
            "{e:?}"
        );
    }

    /// A leading byte-order mark is not part of the first key, and line
    /// numbers still count from the line it sits on. Negative control: a
    /// mark anywhere but the very start is still an unknown key.
    #[test]
    fn parse_theme_ignores_a_leading_byte_order_mark() {
        let plain = "foreground = #ffffff\nbackground = #000000\n";
        let marked = format!("\u{feff}{plain}");
        assert_eq!(parse_theme_str(&marked), parse_theme_str(plain));
        let e = parse_theme_str("\u{feff}foreground = #ffffff\nbogus = 1\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
        let e =
            parse_theme_str("foreground = #ffffff\n\u{feff}background = #000000\n").unwrap_err();
        assert!(matches!(e, ThemeError::Parse { line: 2, .. }), "{e:?}");
    }

    /// The canonical key, then any alias of the same key, is a duplicate whose
    /// error names the second line; every scalar key and every alias pair is
    /// covered. Negative control: each key set once, in any spelling, parses.
    #[test]
    fn parse_theme_duplicate_keys_fold_aliases() {
        let e = parse_scheme_str("foreground = #ffffff\nfg = #000000\nbackground = #000000\n")
            .unwrap_err();
        assert!(
            matches!(&e, ThemeError::Parse { line: 2, reason, .. }
                if reason.contains("duplicate") && reason.contains("line 1")),
            "{e:?}"
        );
        // Every spelling of every scalar key, paired with every spelling of the
        // same key: always a duplicate on the second line.
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
        // Negative control: each key once, under its alternate spelling, parses.
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

    /// The `aterm.toml` spellings are the same slots as the short ones: a file
    /// written either way parses to the same scheme.
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
        // Built-ins and Default carry none.
        assert_eq!(ColorScheme::default().selection_fg, None);
        for name in builtin_names() {
            assert_eq!(builtin(name).unwrap().selection_fg, None, "{name}");
        }
    }

    /// `#RGB` widens each digit to a byte, for every digit, with or without
    /// `#`; any other length is refused.
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
        // The bare three-digit spelling is a word, not a colour.
        for bad in [
            "", "#", "#a", "#ab", "#abcd", "#abcde", "#abcdefa", "#ggg", "#é12", "bad", "fed",
            "abc",
        ] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        let s = parse_scheme_str("fg = #abc\nbg = #000\n").expect("#RGB colours parse");
        assert_eq!(s.foreground, Rgb::new(0xaa, 0xbb, 0xcc));
        assert_eq!(s.background, Rgb::new(0, 0, 0));
    }

    /// `name`, `description` and `author` are bounded and free of control
    /// characters; at the bound they are kept verbatim.
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

    /// Other terminals' theme files fail to parse and the error names their
    /// format and `aterm theme import`; a native file's own error carries no
    /// hint (negative control).
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
        // Negative control: native files, valid or not, are not foreign.
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

    /// An unknown key stays fatal, including Ghostty's keys and the key names
    /// of other terminals that aterm does not read natively.
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
            "bold_is_bright",
        ] {
            let e = parse_scheme_str(&format!("{base}{key} = #123456\n")).unwrap_err();
            assert!(
                matches!(&e, ThemeError::Parse { line: 3, reason, .. } if reason.contains("unknown key")),
                "{key}: {e:?}"
            );
        }
    }

    /// Inline comments after a value are stripped; a leading `#` hex value is kept.
    #[test]
    fn parse_scheme_str_inline_comments() {
        let s = parse_scheme_str("foreground = #abcdef  # the fg\nbackground = #001122 ; bg\n")
            .expect("parses with inline comments");
        assert_eq!(s.foreground, Rgb::new(0xab, 0xcd, 0xef));
        assert_eq!(s.background, Rgb::new(0x00, 0x11, 0x22));
    }

    /// No spelling (name or alias, stem or plain) belongs to two rows. (That
    /// every key has its row is the compiler's: `ThemeKey::row` is exhaustive
    /// and a const assertion checks the table's order.)
    #[test]
    fn no_theme_key_spelling_names_two_keys() {
        let mut spellings = std::collections::HashSet::new();
        for spec in &THEME_KEYS {
            for spelling in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
                assert!(spellings.insert(spelling), "{spelling} is two keys");
            }
        }
    }

    /// The file a writer builds from the table's examples, every key spelled
    /// canonically (every slot of an indexed key), with `skip` left out.
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

    /// The parser reads keys by the table: a file of the table's examples
    /// parses, under canonical names and aliases alike; leaving out a key the
    /// table marks required is refused with that key named, and leaving out
    /// any other key is not. Every example is a value of its key's type.
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
        // Negative control: a key outside the table is still refused.
        let mut extra = file_from_examples(None, false);
        extra.push_str("colour16 = #000000\n");
        assert!(matches!(
            parse_theme_str(&extra),
            Err(ThemeError::Parse { key: Some(ref k), .. }) if k == "colour16"
        ));
        assert_eq!(lookup_theme_key("color+1"), None);
        assert_eq!(lookup_theme_key("color16"), None);
        assert_eq!(
            lookup_theme_key("ANSI15").map(|(_, slot)| slot),
            Some(Some(15))
        );
    }

    /// `parse_theme_located` says the line each key was set on, under any
    /// spelling, and `None` for a key the file leaves out.
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

    /// The admission contrast warnings are exactly the contrast rows below
    /// their bar, over every built-in and a sweep of synthetic themes; the
    /// sweep includes themes that earn each warning (the negative control).
    #[test]
    fn contrast_warnings_are_the_report_rows_below_their_bar() {
        let mut schemes: Vec<ColorScheme> =
            builtin_names().into_iter().filter_map(builtin).collect();
        for grey in (0u8..=255).step_by(17) {
            schemes.push(ColorScheme {
                foreground: Rgb::new(grey, grey, grey),
                selection: Some(Rgb::new(grey / 2, grey / 2, grey)),
                cursor: (grey % 2 == 0).then_some(Rgb::new(grey, 0x20, 0x20)),
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
                    other => panic!("{other:?} has no bar"),
                })
                .collect();
            let admitted: Vec<ThemeWarning> = admission_warnings(scheme, scheme.appearance)
                .into_iter()
                .filter(|w| !matches!(w, ThemeWarning::AnsiMatchesBackground { .. }))
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
            assert!((0u8..16).all(|slot| has(ContrastPair::AnsiOnBackground(slot))));
        }
        assert!(
            warned > 0,
            "the sweep must include themes that earn a warning"
        );
    }
}
