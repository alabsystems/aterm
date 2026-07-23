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

/// Whether a scheme is meant for a dark or light background. Drives the optional
/// `dark:…,light:…` auto-split (a future GUI concern); carried here so a scheme
/// is self-describing.
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
    /// Selection highlight bg; `None` uses the renderer's built-in selection color.
    pub selection: Option<Rgb>,
    /// The 16 ANSI slots: `[0..8)` normal (black, red, green, yellow, blue, magenta,
    /// cyan, white), `[8..16)` their bright variants. THE authoritative ANSI table.
    pub ansi: [Rgb; 16],
}

/// The renderer's built-in selection color (`aterm_render::Theme::default().selection`,
/// `#33415E`), used when a scheme leaves `selection` as `None`. Verified by the
/// `default_theme_parts_are_historical` test in this module.
///
/// Darker, lower-saturation than the old `#264F78`: selected COLOURED text keeps its
/// own fg, and over `#264F78` mid-tone syntax colours dropped below ~2:1 (magenta,
/// comment-gray, blue) — unreadable while selecting git/ls/log output. `#33415E`
/// darkens the highlight so it recedes behind the text (>~3:1) yet still clearly
/// reads as a selection against the `#111318` background. (LLM-judge finding V3.)
const SELECTION_DEFAULT: Rgb = Rgb::new(0x33, 0x41, 0x5E);

impl Default for ColorScheme {
    /// Byte-identical to aterm's historical look: fg `#D0D0D0`, bg `#111318`, cursor
    /// `#50FA7B`, selection `#264F78`, and `ansi[i] == ColorPalette::default_color(i)`
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
    /// to the foreground and `selection` to the renderer default when unset.
    #[must_use]
    pub fn to_theme_parts(&self) -> ThemeParts {
        let pack = |c: Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        ThemeParts {
            fg: pack(self.foreground),
            bg: pack(self.background),
            cursor: pack(self.cursor.unwrap_or(self.foreground)),
            selection: pack(self.selection.unwrap_or(SELECTION_DEFAULT)),
        }
    }
}

/// A compile-time built-in scheme: all-`Rgb` (const-constructible) so the registry
/// lives in `.rodata` with no allocation. Materialized into a [`ColorScheme`] (which
/// owns a `String` name) on lookup.
struct Builtin {
    name: &'static str,
    appearance: Appearance,
    fg: Rgb,
    bg: Rgb,
    cursor: Rgb,
    selection: Rgb,
    ansi: [Rgb; 16],
}

impl Builtin {
    fn to_scheme(&self) -> ColorScheme {
        ColorScheme {
            name: self.name.to_string(),
            appearance: self.appearance,
            foreground: self.fg,
            background: self.bg,
            cursor: Some(self.cursor),
            selection: Some(self.selection),
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
    // Palette values verified against github.com/mbadolato/iTerm2-Color-Schemes and
    // each theme's own spec. NOTE: Nord and Catppuccin's canonical files list a
    // near-WHITE selection (their selection-*foreground*); aterm paints selection as
    // a background only, so those are replaced with each theme's proper dark
    // selection surface (Nord nord3 #4c566a, Catppuccin Surface2 #585b70).
    Builtin {
        name: "Dracula",
        appearance: Appearance::Dark,
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
        fg: c(0xc0, 0xca, 0xf5),
        bg: c(0x1a, 0x1b, 0x26),
        // Accent blue, not the fg lavender — a fg-coloured full-cell block reads as a
        // pale, heavy slab (visual-judge: "heavier than the glyphs"). The blue cursor
        // reads as a cursor and stays distinct from text.
        cursor: c(0x7a, 0xa2, 0xf7),
        selection: c(0x28, 0x34, 0x57),
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
        fg: c(0xeb, 0xdb, 0xb2),
        bg: c(0x28, 0x28, 0x28),
        cursor: c(0xeb, 0xdb, 0xb2),
        selection: c(0x66, 0x5c, 0x54),
        ansi: [
            c(0x28, 0x28, 0x28),
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
        fg: c(0x83, 0x94, 0x96),
        bg: c(0x00, 0x2b, 0x36),
        cursor: c(0x83, 0x94, 0x96),
        selection: c(0x07, 0x36, 0x42),
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
        fg: c(0xab, 0xb2, 0xbf),
        bg: c(0x21, 0x25, 0x2b),
        cursor: c(0xab, 0xb2, 0xbf),
        selection: c(0x32, 0x38, 0x44),
        ansi: [
            c(0x21, 0x25, 0x2b),
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
    // *foreground* (they rely on a selection-foreground swap aterm doesn't do — it
    // paints selection as a BACKGROUND only). Those would render selected text
    // invisibly (dark-on-dark), so each is replaced with that theme's proper LIGHT
    // selection surface — the mirror of the Nord/Catppuccin dark-selection fix above.
    // The `selection_is_legible_over_foreground` test guards every substitution.
    Builtin {
        name: "Solarized Light",
        appearance: Appearance::Light,
        fg: c(0x65, 0x7b, 0x83),
        bg: c(0xfd, 0xf6, 0xe3),
        cursor: c(0x65, 0x7b, 0x83),
        selection: c(0xee, 0xe8, 0xd5), // base2 — upstream value, already a light surface
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
            c(0xfd, 0xf6, 0xe3),
        ],
    },
    Builtin {
        name: "Gruvbox Light",
        appearance: Appearance::Light,
        fg: c(0x3c, 0x38, 0x36),
        bg: c(0xfb, 0xf1, 0xc7),
        cursor: c(0x3c, 0x38, 0x36),
        // Upstream selection is the dark fg #3c3836 (invisible as a bg highlight);
        // replaced with Gruvbox light3 #bdae93 — the mirror of dark3 #665c54 used by
        // Gruvbox Dark above.
        selection: c(0xbd, 0xae, 0x93),
        ansi: [
            c(0xfb, 0xf1, 0xc7),
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
        fg: c(0x4c, 0x4f, 0x69),
        bg: c(0xef, 0xf1, 0xf5),
        cursor: c(0xdc, 0x8a, 0x78), // Rosewater (mirrors Mocha's Rosewater cursor)
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
        fg: c(0x1f, 0x23, 0x28),
        bg: c(0xff, 0xff, 0xff),
        cursor: c(0x09, 0x69, 0xda), // GitHub accent blue — upstream value, highly legible
        // Upstream selection is the dark fg #1f2328 (invisible as a bg highlight);
        // replaced with Primer blue-1 #b6e3ff, GitHub's own light selection tint.
        selection: c(0xb6, 0xe3, 0xff),
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
    if name.eq_ignore_ascii_case("default") {
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
    v.push("Default");
    v.extend(BUILTINS.iter().map(|b| b.name));
    v
}

/// One-line human description for a built-in scheme name (matches the palette
/// comments above). Falls back to a generic line for an unknown name so this can
/// never panic. Pairs with [`builtin_names`] / [`builtin_themes`].
#[must_use]
pub fn builtin_description(name: &str) -> &'static str {
    match name {
        "Default" => "aterm's historical dark look (fg #D0D0D0, green cursor)",
        "Dracula" => "vibrant dark theme — the Dracula palette",
        "Nord" => "arctic, north-bluish palette for dark UIs",
        "Tokyo Night" => "deep blue/purple dark theme",
        "Catppuccin Mocha" => "warm, soothing dark theme (Catppuccin mocha)",
        "Gruvbox Dark" => "retro-groove dark theme, warm earthy colours",
        "Solarized Dark" => "dark variant of the Solarized precision palette",
        "One Dark" => "Atom One Dark — atom-inspired dark theme",
        "Solarized Light" => "light variant of the Solarized precision palette",
        "Gruvbox Light" => "retro-groove light theme, warm earthy colours",
        "Catppuccin Latte" => "warm, soothing light theme (Catppuccin latte)",
        "GitHub Light" => "GitHub's light UI palette — crisp on white",
        _ => "a built-in colour scheme",
    }
}

/// All built-in schemes as `(name, description)` pairs, `"Default"` first — the
/// data behind `aterm list-themes`. Reuses [`builtin_names`] so the set can't
/// drift from the registry.
#[must_use]
pub fn builtin_themes() -> Vec<(&'static str, &'static str)> {
    builtin_names()
        .into_iter()
        // `.take(64)` is identity — `builtin_names` yields `BUILTINS.len() + 1`
        // entries (about a dozen, and its own capacity clamp already assumes
        // `<= 64`) — but it hands the `collect` a literal element bound so the
        // Trust gate can prove the allocation bounded.
        .take(64)
        .map(|name| (name, builtin_description(name)))
        .collect()
}

/// Error from parsing a disk theme definition ([`parse_scheme_str`]) or loading a
/// theme by name ([`load`]). Stringly-typed line context so a bad user theme file
/// produces an actionable diagnostic rather than a silent fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    /// `load(name)`: neither a built-in nor a file at any candidate path matched.
    /// Carries the name the user asked for.
    NotFound(String),
    /// The file existed but could not be read (permissions, I/O). Carries the
    /// `Display` of the underlying error.
    Io(String),
    /// A line was syntactically malformed (no `=`, bad colour, unknown key), or a
    /// required key was missing.
    Parse {
        /// 1-based line number of the offending line (`0` for a whole-file problem
        /// such as a missing required key).
        line: usize,
        /// Short human-readable reason for the failure.
        reason: String,
    },
}

impl core::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Trust gate: `write_str` instead of `write!` — runtime-argument
        // `format_args!` cannot be lowered natively. Byte-identical: nested
        // `{…}` always formats with default options, exactly like
        // `to_string`/`debug_string`.
        match self {
            ThemeError::NotFound(name) => {
                f.write_str("theme ")?;
                f.write_str(&crate::trust_fmt::debug_string(name))?;
                f.write_str(" not found (no built-in and no theme file)")
            }
            ThemeError::Io(e) => {
                f.write_str("theme file read error: ")?;
                f.write_str(e)
            }
            ThemeError::Parse { line, reason } => {
                f.write_str("theme parse error on line ")?;
                f.write_str(&line.to_string())?;
                f.write_str(": ")?;
                f.write_str(reason)
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// Parse a `#RRGGBB` (or bare `RRGGBB`) hex colour. `None` on malformed input.
/// Mirrors the GUI's `app_config::parse_hex_color` but lives here so the loader is
/// self-contained (no GUI dependency in this bottom-of-graph crate).
// Skip: radix parse absent std body; malformed input returns None.
#[cfg_attr(trust_verify, trust::skip)]
fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let h = h.strip_prefix('#').unwrap_or(h);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // `h.len() == 6` was checked above, so each total `get(..)` yields `Some`;
    // this is byte-identical to the old fixed-range slicing but carries no
    // slice-bounds obligation for the Trust gate.
    Some(Rgb::new(
        u8::from_str_radix(h.get(0..2)?, 16).ok()?,
        u8::from_str_radix(h.get(2..4)?, 16).ok()?,
        u8::from_str_radix(h.get(4..6)?, 16).ok()?,
    ))
}

/// Parse a minimal disk theme definition into a [`ColorScheme`].
///
/// FORMAT — a deliberately tiny, dependency-free `key = value` line format (so this
/// bottom-of-graph crate needs no TOML/serde at runtime), mirroring the Ghostty /
/// Alacritty-`.conf` style users already know:
///
/// ```text
/// # comments start with '#' (or ';'); blank lines ignored
/// name       = My Theme         # optional display name
/// appearance = dark             # dark (default) | light
/// foreground = #c0caf5
/// background = #1a1b26
/// cursor     = #7aa2f7          # optional; omitted => follows foreground
/// selection  = #283457          # optional; omitted => renderer default
/// # the 16 ANSI slots, by 0-based index (0-7 normal, 8-15 bright):
/// color0 = #15161e
/// color1 = #f7768e
/// # … through color15. Aliases: `palette0`/`ansi0` also accepted.
/// ```
///
/// SEMANTICS: `foreground` and `background` are REQUIRED (a theme with no fg/bg is
/// rejected). Any unspecified `colorN` defaults to that slot's
/// [`ColorPalette::default_color`], so a partial palette is still well-formed. An
/// unknown key, a malformed colour, or a duplicate key is a [`ThemeError::Parse`]
/// with the 1-based line number. Keys are case-insensitive.
///
/// # Errors
/// Returns [`ThemeError::Parse`] for a malformed line, a bad colour value, an
/// unknown key, or a missing required `foreground`/`background`.
// Skip: the parse closures drive absent std iterator/parse bodies; every malformed input is rejected (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_scheme_str(text: &str) -> Result<ColorScheme, ThemeError> {
    let mut name = String::new();
    let mut appearance = Appearance::Dark;
    let mut foreground: Option<Rgb> = None;
    let mut background: Option<Rgb> = None;
    let mut cursor: Option<Rgb> = None;
    let mut selection: Option<Rgb> = None;
    // Start from the engine defaults so an omitted `colorN` is render-identical to
    // an unthemed slot.
    let mut ansi = [Rgb::new(0, 0, 0); 16];
    for (i, slot) in ansi.iter_mut().enumerate() {
        *slot = ColorPalette::default_color(i as u8);
    }
    // Track which ANSI slots were explicitly set so a duplicate `colorN` is caught.
    let mut ansi_seen = [false; 16];

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
            let mut reason = String::from("missing '=' in ");
            reason.push_str(&crate::trust_fmt::debug_string(line));
            return Err(ThemeError::Parse {
                line: lineno,
                reason,
            });
        };
        let key = key.trim().to_ascii_lowercase();
        let value = strip_inline_comment(value).trim().to_string();
        if value.is_empty() {
            let mut reason = String::from("empty value for key ");
            reason.push_str(&crate::trust_fmt::debug_string(&key));
            return Err(ThemeError::Parse {
                line: lineno,
                reason,
            });
        }

        let bad_color = |reason_key: &str| {
            let mut reason = String::from("invalid #RRGGBB colour for ");
            reason.push_str(&crate::trust_fmt::debug_string(reason_key));
            reason.push_str(": ");
            reason.push_str(&crate::trust_fmt::debug_string(&value));
            ThemeError::Parse {
                line: lineno,
                reason,
            }
        };

        match key.as_str() {
            "name" => name = value,
            "appearance" => {
                appearance = match value.to_ascii_lowercase().as_str() {
                    "dark" => Appearance::Dark,
                    "light" => Appearance::Light,
                    _ => {
                        let mut reason = String::from("appearance must be dark|light, got ");
                        reason.push_str(&crate::trust_fmt::debug_string(&value));
                        return Err(ThemeError::Parse {
                            line: lineno,
                            reason,
                        });
                    }
                };
            }
            "foreground" | "fg" => {
                foreground = Some(parse_hex(&value).ok_or_else(|| bad_color("foreground"))?)
            }
            "background" | "bg" => {
                background = Some(parse_hex(&value).ok_or_else(|| bad_color("background"))?)
            }
            "cursor" => cursor = Some(parse_hex(&value).ok_or_else(|| bad_color("cursor"))?),
            "selection" => {
                selection = Some(parse_hex(&value).ok_or_else(|| bad_color("selection"))?);
            }
            other => {
                // colorN / paletteN / ansiN, N in 0..=15.
                let n = other
                    .strip_prefix("color")
                    .or_else(|| other.strip_prefix("palette"))
                    .or_else(|| other.strip_prefix("ansi"))
                    .and_then(|d| d.parse::<usize>().ok());
                match n {
                    Some(n) if n < 16 => {
                        if ansi_seen[n] {
                            let mut reason = String::from("duplicate colour index ");
                            reason.push_str(&n.to_string());
                            return Err(ThemeError::Parse {
                                line: lineno,
                                reason,
                            });
                        }
                        ansi[n] = parse_hex(&value).ok_or_else(|| bad_color(other))?;
                        ansi_seen[n] = true;
                    }
                    _ => {
                        let mut reason = String::from("unknown key ");
                        reason.push_str(&crate::trust_fmt::debug_string(other));
                        return Err(ThemeError::Parse {
                            line: lineno,
                            reason,
                        });
                    }
                }
            }
        }
    }

    let foreground = foreground.ok_or_else(|| ThemeError::Parse {
        line: 0,
        reason: "missing required key 'foreground'".to_string(),
    })?;
    let background = background.ok_or_else(|| ThemeError::Parse {
        line: 0,
        reason: "missing required key 'background'".to_string(),
    })?;

    Ok(ColorScheme {
        name,
        appearance,
        foreground,
        background,
        cursor,
        selection,
        ansi,
    })
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

/// Resolve a theme by NAME: a built-in (via [`builtin`]) wins; otherwise a user
/// theme file is loaded and parsed via [`parse_scheme_str`].
///
/// FILE LOCATION — a `<name>.conf` file under the user theme directory, which
/// mirrors the GUI config convention (`app_config`): `$XDG_CONFIG_HOME/aterm/themes/`
/// when `XDG_CONFIG_HOME` is set, else `~/.config/aterm/themes/`. (We intentionally
/// use the same `~/.config/aterm` root as `aterm.toml` rather than the platform
/// `dirs::config_dir`, so themes sit beside the config the user already edits.) The
/// name is matched VERBATIM as the file stem, so `load("My Theme")` reads
/// `…/themes/My Theme.conf`. Theme PACKS (e.g. a checkout of
/// github.com/mbadolato/iTerm2-Color-Schemes converted to this format) drop their
/// `*.conf` files straight into that directory.
///
/// # Errors
/// [`ThemeError::NotFound`] if neither a built-in nor a file matched;
/// [`ThemeError::Io`] if the file existed but could not be read;
/// [`ThemeError::Parse`] (via [`parse_scheme_str`]) for a malformed file.
// Skip: fs read + parse (absent std bodies); every failure returns Err (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
pub fn load(name: &str) -> Result<ColorScheme, ThemeError> {
    if let Some(s) = builtin(name) {
        return Ok(s);
    }
    let Some(path) = user_theme_path(name) else {
        return Err(ThemeError::NotFound(name.to_string()));
    };
    match std::fs::read(&path) {
        // Explicit strict UTF-8 decode (`String::from_utf8`, not
        // `read_to_string`'s implicit validation) so the hardened gate sees
        // the reject path. Behavior-identical: valid UTF-8 decodes to the
        // same string, and on invalid UTF-8 `read_to_string` fails with an
        // `ErrorKind::InvalidData` error whose `to_string()` is exactly this
        // message — the only thing callers ever observed through
        // `ThemeError::Io`.
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => parse_scheme_str(&text),
            Err(_) => Err(ThemeError::Io(
                "stream did not contain valid UTF-8".to_string(),
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ThemeError::NotFound(name.to_string()))
        }
        Err(e) => Err(ThemeError::Io(e.to_string())),
    }
}

/// The user theme directory (`<config>/aterm/themes`), or `None` when no config
/// home can be resolved (no `XDG_CONFIG_HOME` and no `HOME`). Public so a packaging
/// / `--list-themes` caller can tell users exactly where to drop theme packs.
#[must_use]
pub fn user_theme_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("themes"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/themes"))
}

/// The candidate file path for a named user theme (`<theme dir>/<name>.conf`), or
/// `None` if no config home is resolvable.
fn user_theme_path(name: &str) -> Option<std::path::PathBuf> {
    // Trust gate: concatenation instead of `format!`; byte-identical.
    user_theme_dir().map(|d| {
        let mut file = String::from(name);
        file.push_str(".conf");
        d.join(file)
    })
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
        // Every name resolves to a scheme, and every description is non-empty and
        // specific (not the generic fallback).
        for (name, desc) in &themes {
            assert!(!desc.is_empty(), "{name} has an empty description");
            assert_ne!(
                *desc,
                builtin_description("__unknown__"),
                "{name} fell back to the generic description"
            );
            if *name != "Default" {
                assert!(builtin(name).is_some(), "{name} must resolve via builtin()");
            }
        }
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
            Some(Rgb::new(0xee, 0xe8, 0xd5))
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
            Some(Rgb::new(0xb6, 0xe3, 0xff))
        );
    }

    /// Every built-in light scheme is actually light (`Appearance::Light` AND a
    /// background brighter than its foreground), and on EVERY built-in — dark or
    /// light — the default foreground stays legible OVER the selection surface
    /// (contrast ≥ 3.0:1). aterm paints selection as a background ONLY, so a scheme
    /// whose `selection` collapses onto the text colour would render selected text
    /// invisibly; this guards the light-theme dark-fg→light-surface substitutions.
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
            // Selection legibility: the selection background (or the renderer default
            // when unset) must hold ≥ 3.0:1 against the default foreground.
            let sel = s.selection.unwrap_or(SELECTION_DEFAULT);
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
        // Short (3-digit) colour is rejected — the format requires #RRGGBB.
        let e = parse_scheme_str("fg=#fff\n").unwrap_err();
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

    /// `load` resolves a built-in by name without ever touching the filesystem.
    #[test]
    fn load_resolves_builtin_first() {
        assert_eq!(load("Default").unwrap(), ColorScheme::default());
        assert_eq!(load("dracula").unwrap().name, "Dracula");
    }

    // The user-theme disk path (`load` reading a file under a scoped
    // `XDG_CONFIG_HOME` override) is covered by `tests/user_theme_env.rs`, NOT
    // here: libtest runs this unit binary's tests in parallel, and
    // `user_theme_dir()` reads the environment on every `load()` call, so an
    // env override in this mod would race every sibling test that calls
    // `load()`. A single `#[test]` alone in its own integration binary owns
    // the process environment legitimately.

    /// Inline comments after a value are stripped; a leading `#` hex value is kept.
    #[test]
    fn parse_scheme_str_inline_comments() {
        let s = parse_scheme_str("foreground = #abcdef  # the fg\nbackground = #001122 ; bg\n")
            .expect("parses with inline comments");
        assert_eq!(s.foreground, Rgb::new(0xab, 0xcd, 0xef));
        assert_eq!(s.background, Rgb::new(0x00, 0x11, 0x22));
    }
}
