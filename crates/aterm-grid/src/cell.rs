// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Packed cell representation (8 bytes).
//!
//! ## Design
//!
//! Extreme compression cell - 8 bytes total (vs previous 12 bytes).
//!
//! Memory savings: 33% reduction
//! For 10,000 lines x 200 cols = 2M cells:
//!   Before: 24 MB
//!   After:  16 MB
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │ char_data (2 bytes)                                            │
//! │   - UTF-16 code unit for BMP (U+0000-U+FFFF)                   │
//! │   - Overflow table index when flags.COMPLEX is set             │
//! ├────────────────────────────────────────────────────────────────┤
//! │ colors (4 bytes) - Packed foreground and background            │
//! │   - Bits 0-7:   FG color index (0-255) or FG mode indicator    │
//! │   - Bits 8-15:  BG color index (0-255) or BG mode indicator    │
//! │   - Bits 16-23: Extra color data / overflow indicator          │
//! │   - Bits 24-31: Color mode flags                               │
//! ├────────────────────────────────────────────────────────────────┤
//! │ flags (2 bytes) - Cell attributes                              │
//! │   - Bits 0-14: Standard attributes                             │
//! │   - Bit 15: COMPLEX flag (char_data is overflow index)         │
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Overflow Tables (in CellExtra)
//!
//! When a cell needs more than 8 bytes can express:
//! - Complex characters (emoji, combining marks, non-BMP): string table
//! - True color RGB: separate fg/bg overflow tables
//! - Hyperlinks: URL storage
//! - Underline color: separate color storage
//!
//! Expected: <1% of cells need overflow.
//!
//! ## Verification
//!
//! - Kani proof: `cell_pack_unpack_roundtrip`
//! - Compile-time assert: `size_of::<Cell>() == 8`

pub use super::cell_colors::{PackedColor, PackedColors};
pub use super::cell_flags::CellFlags;
use super::style::StyleId;

// Constructors (from_ascii_fast, from_ascii_styled, new, with_style,
// with_overflow_index, with_style_id, from_ascii_with_style_id) and color
// conversion helpers are in cell_constructors.rs.

/// A single terminal cell (8 bytes). See module docs for layout details.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, packed)]
pub struct Cell {
    /// Character data.
    /// - BMP characters (U+0000-U+FFFF): UTF-16 code unit directly
    /// - Complex/non-BMP: overflow table index (when COMPLEX flag set)
    char_data: u16,
    /// Packed foreground and background colors.
    colors: PackedColors,
    /// Cell flags including COMPLEX indicator.
    flags: CellFlags,
}

// Compile-time size check - MUST be exactly 8 bytes
const _: () = assert!(std::mem::size_of::<Cell>() == 8);

impl Default for Cell {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Copy packed fields to avoid unaligned reference errors
        let char_data = self.char_data;
        let flags = self.flags;
        let colors = self.colors;
        if flags.is_complex() {
            f.debug_struct("Cell")
                .field("overflow_index", &char_data)
                .field("complex", &true)
                .field("flags", &flags)
                .field("colors", &colors)
                .finish()
        } else {
            let ch = char::from_u32(u32::from(char_data)).unwrap_or('\u{FFFD}');
            f.debug_struct("Cell")
                .field("char", &ch)
                .field("flags", &flags)
                .field("colors", &colors)
                .finish()
        }
    }
}

impl Cell {
    /// Maximum codepoint that fits directly in char_data (BMP).
    pub const MAX_DIRECT_CODEPOINT: u32 = 0xFFFF;

    /// Empty cell (space with default colors).
    pub const EMPTY: Self = Self {
        char_data: ' ' as u16,
        colors: PackedColors::DEFAULT,
        flags: CellFlags::empty(),
    };

    /// Create a BCE (Background Color Erase) blank cell.
    ///
    /// Per VT420/xterm, erase operations fill cells with a space that inherits
    /// the current SGR background color. This cell has a space character,
    /// default foreground, the given background color, and no flags.
    ///
    /// When the background is default, this returns `Cell::EMPTY`.
    #[must_use]
    #[inline]
    pub const fn bce_blank(bg_colors: PackedColors) -> Self {
        // Extract only the BG portion — clear FG mode bits and any extras flag.
        // BG mode is in bits 28-31, BG index is in bits 8-15.
        let bg_only = bg_colors.0 & 0xF000_FF00;
        if bg_only == 0 {
            // Default background — fast path
            Self::EMPTY
        } else {
            Self {
                char_data: ' ' as u16,
                colors: PackedColors(bg_only),
                flags: CellFlags::empty(),
            }
        }
    }

    /// Create a BCE blank cell from a `PackedColor` background.
    ///
    /// Converts the legacy `PackedColor` bg format to `PackedColors` layout
    /// and delegates to `bce_blank`. Handles default, indexed, and RGB modes.
    ///
    /// For RGB mode, the cell will have the RGB mode flag set but the actual
    /// RGB values must be written to `CellExtras` separately at each position.
    #[must_use]
    #[inline]
    pub const fn bce_blank_from_bg(bg: PackedColor) -> Self {
        if bg.is_default() {
            Self::EMPTY
        } else if bg.is_indexed() {
            Self::bce_blank(PackedColors::with_indexed_bg(bg.index()))
        } else {
            // RGB mode: set the bg mode flag. Actual RGB values are in extras.
            Self::bce_blank(PackedColors(0).with_rgb_bg())
        }
    }

    // Constructors in cell_constructors.rs: from_ascii_fast, from_ascii_styled,
    // new, with_style, convert_colors, convert_legacy_colors, with_overflow_index,
    // with_style_id, from_ascii_with_style_id.

    /// Get the raw char_data value.
    ///
    /// If `is_complex()`, this is an overflow table index.
    /// Otherwise, it's a UTF-16 code unit (BMP codepoint).
    #[must_use]
    #[inline]
    pub const fn char_data(&self) -> u16 {
        self.char_data
    }

    /// Check if this cell uses overflow for its character.
    #[must_use]
    #[inline]
    pub const fn is_complex(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        flags.is_complex()
    }

    /// Get the Unicode codepoint (only valid for non-complex cells).
    ///
    /// For complex cells, use the overflow table with `char_data()` as key.
    #[must_use]
    #[inline]
    pub const fn codepoint(&self) -> u32 {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        if flags.is_complex() {
            // Complex cell - return replacement char, caller should use overflow
            0xFFFD
        } else {
            self.char_data as u32
        }
    }

    /// Get the character (may be replacement char if complex or invalid).
    ///
    /// Returns `'\u{FFFD}'` for complex cells that contain non-BMP characters
    /// or grapheme clusters. For those cells, the actual character data is
    /// stored in the overflow table (`CellExtras`).
    ///
    /// For a higher-level API that transparently resolves complex cells,
    /// use [`Grid::resolved_char(row, col)`](crate::Grid::resolved_char).
    #[must_use]
    #[inline]
    pub fn char(&self) -> char {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        let char_data = self.char_data;
        if flags.is_complex() {
            '\u{FFFD}' // Complex - use overflow table
        } else {
            char::from_u32(u32::from(char_data)).unwrap_or('\u{FFFD}')
        }
    }

    /// Get the cell flags.
    #[must_use]
    #[inline]
    pub const fn flags(&self) -> CellFlags {
        self.flags
    }

    /// Get packed colors.
    #[must_use]
    #[inline]
    pub const fn colors(&self) -> PackedColors {
        self.colors
    }

    /// Check if this cell uses StyleId instead of inline colors.
    ///
    /// When true, use `style_id()` to get the StyleId and look up
    /// colors/attributes from the StyleTable.
    #[must_use]
    #[inline]
    pub const fn uses_style_id(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        flags.uses_style_id()
    }

    /// Get the StyleId (only valid when `uses_style_id()` is true).
    ///
    /// The StyleId is stored in the low 16 bits of the colors field.
    /// Use this to look up the actual style from the StyleTable.
    #[must_use]
    #[inline]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "consistent &self API; Cell is exactly 8 bytes (at threshold limit)"
    )]
    pub const fn style_id(&self) -> StyleId {
        // Copy from packed struct to avoid unaligned access
        let colors = self.colors;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "mask 0xFFFF guarantees value fits in u16"
        )]
        StyleId::new((colors.0 & 0xFFFF) as u16)
    }

    /// Get the StyleId if this cell uses style interning, otherwise None.
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    #[inline]
    pub const fn style_id_opt(&self) -> Option<StyleId> {
        if self.uses_style_id() {
            Some(self.style_id())
        } else {
            None
        }
    }

    /// Set the character (BMP only, use overflow for non-BMP).
    ///
    /// For BMP characters (U+0000-U+FFFF), stores the codepoint directly.
    /// For non-BMP characters, stores U+FFFD (replacement character) and
    /// triggers a debug assertion. Callers handling non-BMP characters
    /// should use `set_overflow_index` with the CellExtras overflow table.
    ///
    /// # Verification (Trust L0)
    ///
    /// The non-BMP `debug_assert!` below is an **intentional, documented
    /// caller-misuse contract**: `set_char` only accepts BMP scalar values,
    /// and passing a non-BMP `char` is a bug the caller is expected to avoid
    /// (route it through `set_overflow_index`). Under Trust's strict L0 gate
    /// this surfaces as an accepted refutation — the solver's counterexample
    /// is exactly `cp = 0x10000` (the first non-BMP codepoint), i.e. the
    /// contract boundary itself. It is *not* dischargeable in a
    /// behavior-preserving way: proving it away would require either a
    /// `#[requires(c as u32 <= 0xFFFF)]` precondition (a contract) or deleting
    /// the assert (which changes debug-build behavior — the misuse would no
    /// longer be caught). Both are out of scope for the verify campaign, so
    /// this obligation is documented rather than discharged. The kani proofs
    /// (`cell_kani_proofs.rs`) cover the BMP contract only, mirroring this.
    ///
    /// The masked cast `(cp & 0xFFFF) as u16` is provably lossless
    /// (`x & 0xFFFF <= u16::MAX`) — the full verifier proves the identical
    /// `u32 -> u16` cast pattern at other sites — but on this obligation the
    /// interval engine returns UNKNOWN with "canonical Trust JSON transport
    /// parse failed", a transport-layer gap in the toolchain (not a logic
    /// gap), plausibly downstream of the accepted debug_assert refutation
    /// polluting this proof unit.
    #[inline]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cp verified <= 0xFFFF before cast; mask makes the cast provably lossless"
    )]
    pub(crate) fn set_char(&mut self, c: char) {
        let cp = c as u32;
        debug_assert!(
            cp <= Self::MAX_DIRECT_CODEPOINT,
            "set_char() called with non-BMP character U+{cp:04X}; use set_overflow_index() instead"
        );
        let char_data = if cp <= Self::MAX_DIRECT_CODEPOINT {
            // The mask is a no-op on this path (cp <= 0xFFFF), but makes the
            // truncating cast provably lossless for the L0 verifier.
            (cp & 0xFFFF) as u16
        } else {
            0xFFFD // U+FFFD REPLACEMENT CHARACTER (non-BMP misuse fallback)
        };
        self.char_data = char_data;
        // Clear structural flags: COMPLEX (not a multi-codepoint character),
        // WIDE (set_char always writes a single-width BMP character), and
        // WIDE_CONTINUATION (the cell is no longer a continuation spacer).
        // Without this, DECFRA fill_rect over wide characters leaves stale
        // WIDE flags (#7478), and fill_rect over wide continuation cells
        // leaves stale bit 10 — misread as PROTECTED since
        // WIDE_CONTINUATION and PROTECTED share the same bit (#7641).
        // Copy-modify-write for packed struct.
        let mut flags = self.flags;
        flags.remove(CellFlags::COMPLEX);
        flags.remove(CellFlags::WIDE);
        flags.remove(CellFlags::WIDE_CONTINUATION);
        self.flags = flags;
    }

    /// Set overflow index for complex character.
    #[inline]
    pub(crate) fn set_overflow_index(&mut self, index: u16) {
        self.char_data = index;
        // Copy-modify-write for packed struct
        let mut flags = self.flags;
        flags.insert(CellFlags::COMPLEX);
        self.flags = flags;
    }

    /// Set the flags.
    #[inline]
    pub fn set_flags(&mut self, flags: CellFlags) {
        self.flags = flags;
    }

    /// Set the foreground color (indexed).
    #[cfg(any(test, feature = "testing"))]
    #[inline]
    pub fn set_fg(&mut self, fg: PackedColor) {
        // Copy from packed struct for modification
        let colors = self.colors;
        if fg.is_default() {
            self.colors = colors.set_fg_default();
        } else if fg.is_indexed() {
            self.colors = colors.set_fg_indexed(fg.index());
        } else {
            // RGB - mark as needing overflow
            self.colors = colors.with_rgb_fg();
        }
    }

    /// Set the StyleId for this cell.
    ///
    /// This converts the cell to use style interning. The colors field is
    /// repurposed to store the StyleId, and USES_STYLE_ID flag is set.
    ///
    /// Cell-specific flags (WIDE, WIDE_CONTINUATION, COMPLEX, PROTECTED) are preserved.
    #[cfg(test)]
    #[inline]
    pub(crate) fn set_style_id(&mut self, style_id: StyleId) {
        // Store StyleId in colors field
        self.colors = PackedColors(u32::from(style_id.raw()));
        // Set USES_STYLE_ID flag while preserving cell-specific flags
        let mut flags = self.flags;
        flags.insert(CellFlags::USES_STYLE_ID);
        self.flags = flags;
    }

    /// Clear the StyleId and switch back to inline colors mode.
    ///
    /// This clears the USES_STYLE_ID flag and sets colors to default.
    /// Use this when transitioning a cell from style interning back to inline.
    #[cfg(test)]
    #[inline]
    pub(crate) fn clear_style_id(&mut self) {
        self.colors = PackedColors::DEFAULT;
        let mut flags = self.flags;
        flags.remove(CellFlags::USES_STYLE_ID);
        self.flags = flags;
    }

    /// Check if this is a wide character.
    #[must_use]
    #[inline]
    pub const fn is_wide(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        flags.contains(CellFlags::WIDE)
    }

    /// Check if this is a wide character continuation.
    #[must_use]
    #[inline]
    pub const fn is_wide_continuation(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        flags.contains(CellFlags::WIDE_CONTINUATION)
    }

    /// Check if this cell is protected from selective erase.
    ///
    /// `PROTECTED` and `WIDE_CONTINUATION` share bit 10, so this returns
    /// `true` for both protected cells **and** wide continuation spacers.
    /// Use `Row::is_cell_protected()` for context-aware disambiguation
    /// when iterating over row cells.
    ///
    /// Correct for: normal cells, wide main cells.
    /// False positive for: unprotected wide-continuation spacers.
    #[must_use]
    #[inline]
    pub const fn is_protected(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let flags = self.flags;
        flags.contains(CellFlags::PROTECTED)
    }

    /// Check if this cell has a CellExtras entry (per-cell flag).
    ///
    /// When true, the rendering path should probe the CellExtras HashMap for
    /// this cell. When false, the probe can be skipped entirely.
    #[must_use]
    #[inline]
    pub const fn has_extras(&self) -> bool {
        let colors = self.colors;
        colors.has_extras()
    }

    /// Set or clear the HAS_EXTRAS flag on this cell.
    #[inline]
    pub(crate) fn set_has_extras(&mut self, has: bool) {
        let colors = self.colors;
        self.colors = if has {
            colors.with_extras_flag()
        } else {
            colors.without_extras_flag()
        };
    }

    /// Check if this cell is empty (space with default colors and no special flags).
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        // Copy from packed struct to avoid unaligned access
        let colors = self.colors;
        let flags = self.flags;
        self.char_data == ' ' as u16 && colors.is_default() && flags.0 == 0
    }

    /// Clear the cell to empty.
    #[cfg(test)]
    #[inline]
    fn clear(&mut self) {
        *self = Self::EMPTY;
    }
}

#[path = "cell_constructors.rs"]
mod constructors;

#[path = "cell_color_accessors.rs"]
mod color_accessors;

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;

#[cfg(kani)]
#[path = "cell_kani_proofs.rs"]
mod proofs;
