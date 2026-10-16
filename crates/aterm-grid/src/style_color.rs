// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RGBA color type with ANSI 256-color palette support.

/// RGBA colour. Default is opaque black (0,0,0,255); use `Color::DEFAULT_FG`
/// or `Color::DEFAULT_BG` explicitly.
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

    /// Create a colour from an ANSI 256-colour index: 0-7 standard, 8-15
    /// bright, 16-231 the 6x6x6 cube, 232-255 greyscale. Reads a precomputed
    /// 1KB table.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn from_ansi_256(index: u8) -> Self {
        ANSI_256_TABLE[index as usize]
    }

    /// Compute an ANSI 256-color value (const fn, used to build the static table).
    #[doc(hidden)]
    #[cfg(test)]
    pub const fn compute_ansi_256(index: u8) -> Self {
        // Standard ANSI colors (xterm defaults)
        const ANSI_16: [(u8, u8, u8); 16] = [
            // Must agree with aterm-types ColorPalette::default_color (checked by
            // grid_ansi_table_matches_color_palette below). This is a
            // style-intern cache key; the palette renders.
            (0, 0, 0),       // 0: Black
            (224, 108, 117), // 1: Red       (#E06C75, lifted from #CD0000)
            (0, 205, 0),     // 2: Green
            (205, 205, 0),   // 3: Yellow
            (59, 142, 234),  // 4: Blue      (#3B8EEA)
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

/// Precomputed ANSI 256-colour palette, indexed by palette index.
#[cfg(test)]
static ANSI_256_TABLE: [Color; 256] = {
    let mut table = [Color::new(0, 0, 0); 256];
    let mut i: u16 = 0;
    while i < 256 {
        table[i as usize] = Color::compute_ansi_256(i as u8);
        i += 1;
    }
    table
};
