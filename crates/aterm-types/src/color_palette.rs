// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sparse 256-color indexed palette with SmallVec storage.

use aterm_alloc::SmallVec;

use crate::Rgb;

/// Sparse 256-color indexed palette, modifiable via OSC 4.
#[derive(Clone, PartialEq, Eq)]
pub struct ColorPalette {
    /// The authoritative record.
    overrides: SmallVec<(u8, Rgb), 16>,
    /// Dense resolved value for every index.
    cache: [Rgb; 256],
}

impl core::fmt::Debug for ColorPalette {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ColorPalette")
            .field("overrides", &self.overrides)
            .finish()
    }
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorPalette {
    /// Slot 4, blue: `#3B8EEA` (5.5:1 on the dark bg; xterm's `#0000EE` is
    /// near-invisible there).
    #[cfg(not(windows))]
    const BLUE: Rgb = Rgb::new(59, 142, 234);
    /// Slot 4 on Windows, `#2973D0`, where blue is a background as often as
    /// ink: pwsh draws every directory name as the default foreground on it
    /// (`$PSStyle.FileInfo.Directory` is `ESC[44;1m`), and `Write-Host
    /// -BackgroundColor DarkBlue` writes white on it. `#3B8EEA` gave those
    /// 2.2:1 and 3.4:1; this one keeps the hue and gives 3.05:1 and 4.7:1.
    #[cfg(windows)]
    const BLUE: Rgb = Rgb::new(0x29, 0x73, 0xD0);
    /// Slot 2, green: xterm's `#00CD00`.
    #[cfg(not(windows))]
    const GREEN: Rgb = Rgb::new(0, 205, 0);
    /// Slot 2 on Windows, One Dark's `#98C379` (9.2:1 on the dark
    /// background), the family slots 1 and 5 already come from.
    #[cfg(windows)]
    const GREEN: Rgb = Rgb::new(0x98, 0xC3, 0x79);
    /// Slot 3, yellow: xterm's `#CDCD00`.
    #[cfg(not(windows))]
    const YELLOW: Rgb = Rgb::new(205, 205, 0);
    /// Slot 3 on Windows, One Dark's `#E5C07B` (10.8:1): git's hashes,
    /// `Write-Host -ForegroundColor DarkYellow`, Git Bash's path.
    #[cfg(windows)]
    const YELLOW: Rgb = Rgb::new(0xE5, 0xC0, 0x7B);
    /// Slot 6, cyan: xterm's `#00CDCD`.
    #[cfg(not(windows))]
    const CYAN: Rgb = Rgb::new(0, 205, 205);
    /// Slot 6 on Windows, One Dark's `#56B6C2` (7.8:1): git's hunk headers,
    /// PSReadLine's strings, Git Bash's branch.
    #[cfg(windows)]
    const CYAN: Rgb = Rgb::new(0x56, 0xB6, 0xC2);
    /// Slot 12, bright blue: xterm's `#5C5CFF`.
    #[cfg(not(windows))]
    const BRIGHT_BLUE: Rgb = Rgb::new(92, 92, 255);
    /// Slot 12 on Windows, `#6EA8FE`: slot 4's hue, lighter than it, 7.7:1
    /// on the dark background.
    #[cfg(windows)]
    const BRIGHT_BLUE: Rgb = Rgb::new(0x6E, 0xA8, 0xFE);
    /// Slot 10, bright green: Dracula's `#50FA7B`.
    #[cfg(not(windows))]
    const BRIGHT_GREEN: Rgb = Rgb::new(80, 250, 123);
    /// Slot 10 on Windows, `#BBEC98`: slot 2's One Dark green, lighter
    /// (13.8:1).
    #[cfg(windows)]
    const BRIGHT_GREEN: Rgb = Rgb::new(0xBB, 0xEC, 0x98);
    /// Slot 11, bright yellow: Dracula's `#F1FA8C`.
    #[cfg(not(windows))]
    const BRIGHT_YELLOW: Rgb = Rgb::new(241, 250, 140);
    /// Slot 11 on Windows, `#FFEE99`: slot 3's amber, lighter (15.9:1), at
    /// chroma ×1.1 like the others, its hue turned 16° toward yellow's sRGB
    /// cusp, no further than that chroma needs.
    #[cfg(windows)]
    const BRIGHT_YELLOW: Rgb = Rgb::new(0xFF, 0xEE, 0x99);
    /// Slot 13, bright magenta: Dracula's pink `#FF79C6`.
    #[cfg(not(windows))]
    const BRIGHT_MAGENTA: Rgb = Rgb::new(255, 121, 198);
    /// Slot 13 on Windows, `#E9A3FF`: slot 5's violet, lighter (9.9:1), where
    /// Dracula's `#FF79C6` turned it pink (29° away).
    #[cfg(windows)]
    const BRIGHT_MAGENTA: Rgb = Rgb::new(0xE9, 0xA3, 0xFF);
    /// Slot 14, bright cyan: Dracula's `#8BE9FD`.
    #[cfg(not(windows))]
    const BRIGHT_CYAN: Rgb = Rgb::new(139, 233, 253);
    /// Slot 14 on Windows, `#73DEEC`: slot 6's teal, lighter (11.9:1).
    #[cfg(windows)]
    const BRIGHT_CYAN: Rgb = Rgb::new(0x73, 0xDE, 0xEC);

    /// Standard ANSI colors (indices 0-7).
    #[rustfmt::skip]
    const ANSI_COLORS: [Rgb; 8] = [
        Rgb { r:   0, g:   0, b:   0 }, // 0: Black
        Rgb { r: 224, g: 108, b: 117 }, // 1: Red (#E06C75, ~4.5:1 on the dark bg;
        Self::GREEN,                    // 2: Green
        Self::YELLOW,                   // 3: Yellow
        Self::BLUE,                     // 4: Blue
        Rgb { r: 198, g: 120, b: 221 }, // 5: Magenta (#C678DD ~5:1; xterm's
        Self::CYAN,                     // 6: Cyan
        Rgb { r: 229, g: 229, b: 229 }, // 7: White
    ];

    /// Bright ANSI colors (indices 8-15).
    #[rustfmt::skip]
    const BRIGHT_COLORS: [Rgb; 8] = [
        Rgb { r: 138, g: 143, b: 153 }, // 8:  Bright Black (#8A8F99, ~6:1: muted
        Rgb { r: 255, g: 110, b: 103 }, // 9:  Bright Red     (#FF6E67)
        Self::BRIGHT_GREEN,             // 10: Bright Green
        Self::BRIGHT_YELLOW,            // 11: Bright Yellow
        Self::BRIGHT_BLUE,              // 12: Bright Blue
        Self::BRIGHT_MAGENTA,           // 13: Bright Magenta
        Self::BRIGHT_CYAN,              // 14: Bright Cyan
        Rgb { r: 255, g: 255, b: 255 }, // 15: Bright White
    ];

    /// Create a new color palette with default xterm colors.
    #[must_use]
    pub fn new() -> Self {
        Self {
            overrides: SmallVec::new(),
            cache: DEFAULT_COLOR_TABLE,
        }
    }

    /// The RGB value for an indexed color.
    #[inline]
    #[must_use]
    pub const fn get(&self, index: u8) -> Rgb {
        // bounds obligation unproved, while the primitive cast proves it.
        self.cache[index as usize]
    }

    /// Set the RGB value for an indexed color.
    pub fn set(&mut self, index: u8, color: Rgb) {
        let default = Self::default_color(index);

        let pos = self.overrides.iter().position(|&(idx, _)| idx == index);

        if color == default {
            if let Some(p) = pos {
                self.overrides.swap_remove(p);
            }
        } else if let Some(p) = pos {
            // it is in bounds.
            if let Some(slot) = self.overrides.get_mut(p) {
                slot.1 = color;
            }
        } else {
            self.overrides.push((index, color));
        }

        self.cache[index as usize] = color;
    }

    /// Reset a single color to its default value.
    pub fn reset_color(&mut self, index: u8) {
        if let Some(pos) = self.overrides.iter().position(|&(idx, _)| idx == index) {
            self.overrides.swap_remove(pos);
        }
        self.cache[index as usize] = Self::default_color(index);
    }

    /// Reset the entire palette to defaults.
    pub fn reset(&mut self) {
        self.overrides.clear();
        self.cache = DEFAULT_COLOR_TABLE;
    }

    /// Returns the number of customized (non-default) colors.
    #[inline]
    #[must_use]
    pub const fn overrides_count(&self) -> usize {
        self.overrides.len()
    }

    /// The overridden (index, color) pairs, for tests.
    #[cfg(test)]
    fn overrides(&self) -> &[(u8, Rgb)] {
        &self.overrides
    }

    /// The default color for an index; `const fn` so [`DEFAULT_COLOR_TABLE`] is built at
    /// compile time.
    const fn compute_default_color(index: u8) -> Rgb {
        match index {
            0..=7 => Self::ANSI_COLORS[index as usize],
            8..=15 => Self::BRIGHT_COLORS[index as usize - 8],
            16..=231 => {
                let idx = index - 16;
                let r = idx / 36;
                let g = (idx % 36) / 6;
                let b = idx % 6;
                Rgb::new(
                    if r == 0 { 0 } else { 55 + 40 * r },
                    if g == 0 { 0 } else { 55 + 40 * g },
                    if b == 0 { 0 } else { 55 + 40 * b },
                )
            }
            232..=255 => {
                let gray = 8 + 10 * (index - 232);
                Rgb::new(gray, gray, gray)
            }
        }
    }

    /// Get the default color for an index.
    #[inline]
    #[must_use]
    pub const fn default_color(index: u8) -> Rgb {
        DEFAULT_COLOR_TABLE[index as usize]
    }

    /// Parse an X11 color specification, or `None` if unrecognized.
    #[must_use]
    pub fn parse_color_spec(spec: &str) -> Option<Rgb> {
        if let Some(rest) = spec.strip_prefix("rgbi:") {
            let mut parts = rest.splitn(4, '/');
            let (Some(rp), Some(gp), Some(bp), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return None;
            };

            let r = Self::parse_float_component(rp)?;
            let g = Self::parse_float_component(gp)?;
            let b = Self::parse_float_component(bp)?;

            Some(Rgb::new(r, g, b))
        } else if let Some(rest) = spec.strip_prefix("rgb:") {
            let mut parts = rest.splitn(4, '/');
            let (Some(rp), Some(gp), Some(bp), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return None;
            };

            let r = Self::parse_hex_component(rp)?;
            let g = Self::parse_hex_component(gp)?;
            let b = Self::parse_hex_component(bp)?;

            Some(Rgb::new(r, g, b))
        } else if let Some(rest) = spec.strip_prefix('#') {
            // Non-ASCII is refused: the arms below count one byte per digit.
            if !rest.is_ascii() {
                return None;
            }
            match rest.len() {
                3 => {
                    let r = u8::from_str_radix(rest.get(0..1)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    let g = u8::from_str_radix(rest.get(1..2)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    let b = u8::from_str_radix(rest.get(2..3)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    Some(Rgb::new(r, g, b))
                }
                6 => {
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(2..4)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                9 => {
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(3..5)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(6..8)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                12 => {
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(8..10)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                _ => None,
            }
        } else {
            // No-op / fallback.
            crate::x11_colors::lookup(spec)
        }
    }

    /// Parse a hex component with 1-4 digits, scaling to 8-bit.
    #[cfg_attr(trust_verify, trust::skip)]
    fn parse_hex_component(s: &str) -> Option<u8> {
        if s.is_empty() || s.len() > 4 {
            return None;
        }

        let value = u16::from_str_radix(s, 16).ok()?;

        let scaled = match s.len() {
            1 => (value << 4) | value, // 0-15 -> 0-255
            2 => value,                // 0-255 -> 0-255
            3 => value >> 4,           // 0-4095 -> 0-255
            4 => value >> 8,           // 0-65535 -> 0-255
            _ => return None,
        };

        Some(scaled.try_into().unwrap_or(u8::MAX))
    }

    /// Parse a floating-point color component, clamped to \[0.0, 1.0\] and rounded to 8 bits.
    #[cfg_attr(trust_verify, trust::skip)]
    fn parse_float_component(s: &str) -> Option<u8> {
        let value: f64 = s.parse().ok()?;
        let clamped = value.clamp(0.0, 1.0);
        Some((clamped * 255.0 + 0.5) as u8)
    }

    /// Format a color as an X11 `rgb:RRRR/GGGG/BBBB` specification (16 bits per component).
    #[must_use]
    pub fn format_color_spec(color: Rgb) -> String {
        let r16 = (u16::from(color.r) << 8) | u16::from(color.r);
        let g16 = (u16::from(color.g) << 8) | u16::from(color.g);
        let b16 = (u16::from(color.b) << 8) | u16::from(color.b);
        let mut spec = String::from("rgb:");
        crate::trust_fmt::push_hex4(&mut spec, r16);
        spec.push('/');
        crate::trust_fmt::push_hex4(&mut spec, g16);
        spec.push('/');
        crate::trust_fmt::push_hex4(&mut spec, b16);
        spec
    }
}

/// Default RGB for every 256-color index, built at compile time from
/// [`ColorPalette::compute_default_color`].
static DEFAULT_COLOR_TABLE: [Rgb; 256] = {
    let mut table = [Rgb::new(0, 0, 0); 256];
    let mut i: u16 = 0;
    while i < 256 {
        table[i as usize] = ColorPalette::compute_default_color(i as u8);
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_colors_are_the_xterm_256_table() {
        for (index, rgb) in [
            (0, Rgb::new(0, 0, 0)),
            (7, Rgb::new(229, 229, 229)),
            (8, Rgb::new(138, 143, 153)),
            (15, Rgb::new(255, 255, 255)),
            (16, Rgb::new(0, 0, 0)),
            (231, Rgb::new(255, 255, 255)),
            (196, Rgb::new(255, 0, 0)),
            (232, Rgb::new(8, 8, 8)),
            (255, Rgb::new(238, 238, 238)),
        ] {
            assert_eq!(ColorPalette::default_color(index), rgb, "index {index}");
        }
    }

    /// On Windows blue is ink and a background alike, and bright blue is its
    /// lighter sibling: blue clears 3.5:1 as ink on Default's background, the
    /// default foreground clears 3:1 and white 4.5:1 on it, and bright blue is
    /// lighter than blue at 7:1 or more.
    #[test]
    fn windows_blues_read_as_ink_and_as_a_background() {
        let page = Rgb::new(0x11, 0x13, 0x18);
        let ink = Rgb::new(0xD0, 0xD0, 0xD0);
        let (black, white) = (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
        let serves = |blue: Rgb, bright: Rgb| {
            blue.contrast(page) >= 3.5
                && ink.contrast(blue) >= 3.0
                && white.contrast(blue) >= 4.5
                && bright.contrast(black) > blue.contrast(black)
                && bright.contrast(page) >= 7.0
        };
        let historical = (Rgb::new(59, 142, 234), Rgb::new(92, 92, 255));
        assert!(!serves(historical.0, historical.1));
        let shipped = (
            ColorPalette::default_color(4),
            ColorPalette::default_color(12),
        );
        assert_eq!(serves(shipped.0, shipped.1), cfg!(windows), "{shipped:?}");
        assert_eq!(shipped == historical, !cfg!(windows));
    }

    /// On Windows the normal chromatic tones other than blue (which is tuned
    /// as a background) are one family: green, yellow and cyan are One
    /// Dark's, like red and magenta, so a diff's additions (green) carry the
    /// weight its deletions (red) do.
    #[test]
    fn windows_normal_tones_are_one_family() {
        let page = Rgb::new(0x11, 0x13, 0x18);
        let saturation = |c: Rgb| {
            let max = c.r.max(c.g).max(c.b);
            let min = c.r.min(c.g).min(c.b);
            f32::from(max - min) / f32::from(max.max(1))
        };
        let family = |slots: [Rgb; 5]| {
            slots
                .iter()
                .all(|&c| saturation(c) <= 0.7 && c.contrast(page) >= 5.0)
        };
        let [red, magenta] = [1, 5].map(ColorPalette::default_color);
        let xterm = [
            red,
            Rgb::new(0, 205, 0),
            Rgb::new(205, 205, 0),
            magenta,
            Rgb::new(0, 205, 205),
        ];
        assert!(!family(xterm), "control: xterm's primaries");
        let shipped = [1, 2, 3, 5, 6].map(ColorPalette::default_color);
        assert_eq!(family(shipped), cfg!(windows), "{shipped:?}");
        assert_eq!(shipped == xterm, !cfg!(windows));
    }

    /// On Windows each chromatic bright is its normal, lighter, so bold-as-bright
    /// text changes brightness and not hue: the same `OkLCh` hue within 10°
    /// (red, blue and the One Dark four), and for green, yellow, magenta and
    /// cyan a lightness step of +0.10 to +0.14 that stands the pair apart
    /// (`OkLab` 0.11 or more and 1.45:1, where the others need 0.05) and keeps
    /// at least 0.85 of the normal's chroma.
    #[test]
    fn windows_brights_are_their_normals_lighter() {
        use crate::scheme_signature::{chroma_hue, oklab, oklab_distance};
        let page = Rgb::new(0x11, 0x13, 0x18);
        let text = [
            ColorPalette::default_color(7),
            ColorPalette::default_color(15),
            crate::ColorScheme::default().foreground,
        ];
        let pair_serves = |slot: u8, normal: Rgb, bright: Rgb| {
            let ((c_normal, h_normal), (c_bright, h_bright)) =
                (chroma_hue(normal), chroma_hue(bright));
            let turn = (h_bright - h_normal + 180.0).rem_euclid(360.0) - 180.0;
            let hue_held = if slot == 3 {
                (0.0..=20.0).contains(&turn)
            } else {
                turn.abs() <= 10.0
            };
            let step = oklab(bright).0 - oklab(normal).0;
            let one_dark = matches!(slot, 2 | 3 | 5 | 6);
            let stepped = !one_dark
                || ((0.10..=0.14).contains(&step)
                    && bright.contrast(normal) >= 1.45
                    && c_bright >= 0.85 * c_normal);
            let apart = if one_dark { 0.11 } else { 0.05 };
            hue_held
                && stepped
                && oklab_distance(normal, bright) >= apart
                && text.iter().all(|&t| oklab_distance(t, bright) >= 0.09)
                && bright.contrast(page) >= 4.5
                && bright.contrast(page) > normal.contrast(page)
        };
        let serves = |brights: [Rgb; 6]| {
            (1..=6u8)
                .zip(brights)
                .all(|(slot, bright)| pair_serves(slot, ColorPalette::default_color(slot), bright))
        };
        let shipped = [9, 10, 11, 12, 13, 14].map(ColorPalette::default_color);
        let dracula = [
            shipped[0],
            Rgb::new(0x50, 0xFA, 0x7B),
            Rgb::new(0xF1, 0xFA, 0x8C),
            shipped[3],
            Rgb::new(0xFF, 0x79, 0xC6),
            Rgb::new(0x8B, 0xE9, 0xFD),
        ];
        if cfg!(windows) {
            let one_dark_pro = [
                Rgb::new(0xAE, 0xDE, 0x8B),
                Rgb::new(0xFF, 0xDA, 0x97),
                Rgb::new(0xE4, 0x8C, 0xFE),
                Rgb::new(0x65, 0xD1, 0xDF),
            ];
            for (slot, (bright, close)) in [2u8, 3, 5, 6].into_iter().zip(
                [dracula[1], dracula[2], dracula[4], dracula[5]]
                    .into_iter()
                    .zip(one_dark_pro),
            ) {
                let normal = ColorPalette::default_color(slot);
                assert!(
                    !pair_serves(slot, normal, bright),
                    "control: Dracula's slot {}",
                    slot + 8
                );
                assert!(
                    !pair_serves(slot, normal, close),
                    "control: One Dark Pro's slot {}",
                    slot + 8
                );
            }
            let cream = Rgb::new(0xFF, 0xEA, 0xC6);
            assert!(
                !pair_serves(3, ColorPalette::default_color(3), cream),
                "control: amber's own hue at +0.12, a cream"
            );
        }
        assert_eq!(serves(shipped), cfg!(windows), "{shipped:?}");
        assert_eq!(shipped == dracula, !cfg!(windows));
    }

    #[test]
    fn parse_color_spec_reads_every_xparsecolor_form() {
        for (spec, rgb) in [
            ("rgb:ff/00/80", Rgb::new(255, 0, 128)),
            ("rgb:f/0/8", Rgb::new(255, 0, 136)),
            ("#f08", Rgb::new(255, 0, 136)),
            ("#ff0080", Rgb::new(255, 0, 128)),
            ("#fff000888", Rgb::new(255, 0, 136)),
            ("#ffff00008080", Rgb::new(255, 0, 128)),
            ("rgbi:1.0/0.5/0.0", Rgb::new(255, 128, 0)),
            ("rgbi:0.0/0.0/0.0", Rgb::new(0, 0, 0)),
            ("rgbi:1.0/1.0/1.0", Rgb::new(255, 255, 255)),
            ("rgbi:1/0/0", Rgb::new(255, 0, 0)),
            ("rgbi:2.0/1.5/1.0", Rgb::new(255, 255, 255)),
            ("rgbi:-0.5/-1.0/0.0", Rgb::new(0, 0, 0)),
            ("rgbi:0.333/0.667/0.5", Rgb::new(85, 170, 128)),
        ] {
            assert_eq!(ColorPalette::parse_color_spec(spec), Some(rgb), "{spec}");
        }
    }

    #[test]
    fn parse_color_spec_refuses_malformed_specs() {
        for spec in [
            "",
            "#",
            "#ff",
            "#ffff",
            "rgb:",
            "rgb:ff/00",
            "rgb:gg/00/00",
            "notacolor",
            "rgbi:",
            "rgbi:1.0/0.5",
            "rgbi:abc/0.0/0.0",
            "rgbi:1.0/0.5/0.0/0.5",
            "#你好",
            "#你",
        ] {
            assert_eq!(ColorPalette::parse_color_spec(spec), None, "{spec:?}");
        }
    }

    #[test]
    fn parse_named_color_reads_x11_and_css_names() {
        for (name, rgb) in [
            ("black", Rgb::new(0, 0, 0)),
            ("silver", Rgb::new(192, 192, 192)),
            ("gray", Rgb::new(128, 128, 128)),
            ("white", Rgb::new(255, 255, 255)),
            ("maroon", Rgb::new(128, 0, 0)),
            ("red", Rgb::new(255, 0, 0)),
            ("purple", Rgb::new(128, 0, 128)),
            ("fuchsia", Rgb::new(255, 0, 255)),
            ("green", Rgb::new(0, 128, 0)),
            ("lime", Rgb::new(0, 255, 0)),
            ("olive", Rgb::new(128, 128, 0)),
            ("yellow", Rgb::new(255, 255, 0)),
            ("navy", Rgb::new(0, 0, 128)),
            ("blue", Rgb::new(0, 0, 255)),
            ("teal", Rgb::new(0, 128, 128)),
            ("aqua", Rgb::new(0, 255, 255)),
            ("coral", Rgb::new(255, 127, 80)),
            ("rebeccapurple", Rgb::new(102, 51, 153)),
        ] {
            assert_eq!(ColorPalette::parse_color_spec(name), Some(rgb), "{name}");
        }
    }

    /// The OSC 4/10/11 reply spelling, and parse(format(c)) == c.
    #[test]
    fn format_color_spec_is_the_16_bit_spelling_and_round_trips() {
        assert_eq!(
            ColorPalette::format_color_spec(Rgb::new(0, 0, 0)),
            "rgb:0000/0000/0000"
        );
        assert_eq!(
            ColorPalette::format_color_spec(Rgb::new(255, 255, 255)),
            "rgb:ffff/ffff/ffff"
        );
        for r in [0u8, 1, 127, 128, 255] {
            for g in [0u8, 1, 127, 128, 255] {
                for b in [0u8, 1, 127, 128, 255] {
                    let original = Rgb::new(r, g, b);
                    let spec = ColorPalette::format_color_spec(original);
                    let parsed = ColorPalette::parse_color_spec(&spec).unwrap();
                    assert_eq!(
                        parsed, original,
                        "roundtrip failed for ({r}, {g}, {b}): {spec}"
                    );
                }
            }
        }
    }

    /// The dense cache agrees with `overrides` at every index after every mutation, including
    /// removing a middle entry, where `swap_remove` moves another override into its slot.
    #[test]
    fn palette_cache_agrees_with_overrides_after_mutations() {
        fn assert_coherent(p: &ColorPalette, what: &str, overrides: usize) {
            assert_eq!(p.overrides_count(), overrides, "{what}: override count");
            for i in 0..=255u8 {
                let expected = p
                    .overrides()
                    .iter()
                    .find(|(idx, _)| *idx == i)
                    .map_or_else(|| ColorPalette::default_color(i), |(_, c)| *c);
                assert_eq!(p.get(i), expected, "{what}: index {i}");
            }
        }

        let mut p = ColorPalette::new();
        assert_coherent(&p, "fresh", 0);

        for i in [0u8, 7, 15, 42, 200, 255] {
            p.set(i, Rgb::new(i ^ 0x5A, i.wrapping_add(9), 3));
        }
        assert_coherent(&p, "after sets", 6);

        p.set(7, Rgb::new(1, 1, 1));
        assert_eq!(p.get(7), Rgb::new(1, 1, 1));
        assert_coherent(&p, "after in-place update", 6);

        p.reset_color(15);
        assert_coherent(&p, "after reset_color of a middle entry", 5);

        p.set(42, ColorPalette::default_color(42));
        assert_coherent(&p, "after set-to-default", 4);

        p.reset_color(101);
        assert_coherent(&p, "after reset_color of a non-override", 4);

        p.reset();
        assert_coherent(&p, "after reset", 0);
    }
}
