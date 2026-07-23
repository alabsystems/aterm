// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RGBA color type with ANSI 256-color palette support.

/// RGBA color.
///
/// Note: Default is black (0,0,0,255), not zero values.
/// Use `Color::DEFAULT_FG` for white or `Color::DEFAULT_BG` for black explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Color {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
    /// Alpha component.
    pub a: u8,
}

impl Default for Color {
    /// Default color is black (used for background).
    fn default() -> Self {
        Self::DEFAULT_BG
    }
}

impl Color {
    /// Create a new opaque color.
    #[must_use]
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Default foreground color (white).
    pub const DEFAULT_FG: Self = Self::new(255, 255, 255);

    /// Default background color (black).
    pub const DEFAULT_BG: Self = Self::new(0, 0, 0);

    /// Get RGB components as a tuple.
    #[must_use]
    #[inline]
    pub const fn to_rgb(self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }

    /// Create a color from an ANSI 256-color index.
    ///
    /// Color indices map as follows:
    /// - 0-7: Standard colors (black, red, green, yellow, blue, magenta, cyan, white)
    /// - 8-15: Bright colors (bright versions of 0-7)
    /// - 16-231: 6x6x6 color cube
    /// - 232-255: Grayscale (dark to light)
    ///
    /// Uses a pre-computed lookup table (1KB, L1d-resident) instead of
    /// per-call arithmetic. Eliminates 3 divisions + 3 modulos for the
    /// 216-color cube range (indices 16-231).
    #[must_use]
    #[inline]
    pub const fn from_ansi_256(index: u8) -> Self {
        ANSI_256_TABLE[index as usize]
    }

    /// Compute an ANSI 256-color value (const fn, used to build the static table).
    #[doc(hidden)]
    pub const fn compute_ansi_256(index: u8) -> Self {
        // Standard ANSI colors (xterm defaults)
        const ANSI_16: [(u8, u8, u8); 16] = [
            // This table MUST agree with the authoritative aterm-types
            // ColorPalette::default_color (enforced by the
            // grid_ansi_table_matches_color_palette test below). It is a
            // style-intern CACHE KEY, never a pixel — the palette renders. The
            // red/magenta/bright row were tempered away from raw neon primaries
            // for contrast/harmony on a dark bg (two LLM judges); keep in sync.
            (0, 0, 0),       // 0: Black
            (224, 108, 117), // 1: Red       (#E06C75, lifted from #CD0000)
            (0, 205, 0),     // 2: Green
            (205, 205, 0),   // 3: Yellow
            (59, 142, 234),  // 4: Blue      (#3B8EEA, lifted from #0000EE)
            (198, 120, 221), // 5: Magenta   (#C678DD, lifted from #CD00CD)
            (0, 205, 205),   // 6: Cyan
            (229, 229, 229), // 7: White
            (138, 143, 153), // 8: Bright Black/Gray (#8A8F99, lifted from #7F7F7F)
            (255, 110, 103), // 9: Bright Red     (#FF6E67)
            (80, 250, 123),  // 10: Bright Green  (#50FA7B)
            (241, 250, 140), // 11: Bright Yellow (#F1FA8C)
            (92, 92, 255),   // 12: Bright Blue
            (255, 121, 198), // 13: Bright Magenta (#FF79C6)
            (139, 233, 253), // 14: Bright Cyan   (#8BE9FD)
            (255, 255, 255), // 15: Bright White
        ];

        if index < 16 {
            let (r, g, b) = ANSI_16[index as usize];
            Self::new(r, g, b)
        } else if index < 232 {
            // 6x6x6 color cube (indices 16-231)
            let idx = index - 16;
            let r = if idx / 36 == 0 {
                0
            } else {
                55 + (idx / 36) * 40
            };
            let g = if (idx % 36) / 6 == 0 {
                0
            } else {
                55 + ((idx % 36) / 6) * 40
            };
            let b = if idx.is_multiple_of(6) {
                0
            } else {
                55 + (idx % 6) * 40
            };
            Self::new(r, g, b)
        } else {
            // Grayscale (indices 232-255)
            let gray = 8 + (index - 232) * 10;
            Self::new(gray, gray, gray)
        }
    }
}

/// Pre-computed ANSI 256-color palette (1KB, compile-time evaluated).
///
/// Eliminates per-call division/modulo arithmetic for the 216-color cube
/// (indices 16-231). Array index == palette index for O(1) lookup.
static ANSI_256_TABLE: [Color; 256] = {
    let mut table = [Color::new(0, 0, 0); 256];
    let mut i: u16 = 0;
    while i < 256 {
        table[i as usize] = Color::compute_ansi_256(i as u8);
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    /// This grid ANSI table is a style-intern CACHE KEY; the table that actually
    /// reaches pixels is the authoritative `aterm_types::ColorPalette`. They MUST
    /// agree on every one of the 256 indices — otherwise an SGR-coloured cell's
    /// intern key encodes a different colour than the one rendered. Index 4 (Blue)
    /// diverged historically (#0000EE here vs #3B8EEA in the palette); this guard
    /// pins all 256 so the two tables can never silently drift again.
    #[test]
    fn grid_ansi_table_matches_color_palette() {
        for i in 0u16..=255 {
            let idx = i as u8;
            let g = Color::from_ansi_256(idx).to_rgb();
            let c = aterm_types::ColorPalette::default_color(idx);
            assert_eq!(
                g,
                (c.r, c.g, c.b),
                "ANSI index {idx} diverges: grid {g:?} vs palette ({},{},{})",
                c.r,
                c.g,
                c.b
            );
        }
    }
}
