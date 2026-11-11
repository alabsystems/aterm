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

    /// The heap bytes this palette holds past its own size: its overrides once more than 16 spill them out of line.
    #[inline]
    #[must_use]
    pub const fn heap_bytes(&self) -> usize {
        if self.overrides.spilled() {
            self.overrides
                .capacity()
                .saturating_mul(core::mem::size_of::<(u8, Rgb)>())
        } else {
            0
        }
    }

    /// The RGB value for an indexed color.
    #[inline]
    #[must_use]
    pub const fn get(&self, index: u8) -> Rgb {
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
            // `p` came from `position` on this vec; the total `get_mut` shows the Trust gate
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
            // Non-hex bytes are refused: the arms below count one hex digit per byte.
            if !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
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
            crate::x11_colors::lookup(spec)
        }
    }

    /// Parse a hex component with 1-4 digits, scaling to 8-bit.
    #[cfg_attr(trust_verify, trust::skip)]
    fn parse_hex_component(s: &str) -> Option<u8> {
        if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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
        if s.starts_with('+') {
            return None;
        }
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
#[path = "color_palette_tests.rs"]
mod tests;
