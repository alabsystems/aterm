// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FFI types for grid cell data.
//!
//! Defines `AtermCell`, `AtermResolvedStyle`, and the opaque `AtermGrid` handle
//! used by C consumers of the terminal grid.

use crate::grid::{Cell, Grid, PackedColor};

/// Opaque grid handle.
/// Widened from `pub(crate)` to `pub` for cross-crate FFI extraction (#2584).
//
// repr/ABI surface consumed by the cfg(test) `ffi_impl`, the test_support
// FFI helpers, the GPU bridge, and the out-of-crate FFI extraction; inert (never
// constructed) in the default lib build.
#[allow(
    dead_code,
    reason = "FFI ABI surface consumed by the test/FFI/GPU layer"
)]
pub struct AtermGrid(pub Grid);

/// Cell data for FFI.
#[allow(
    dead_code,
    reason = "FFI ABI surface consumed by the test/FFI/GPU layer"
)]
#[repr(C)]
pub struct AtermCell {
    /// Unicode codepoint (0 for empty cell).
    pub codepoint: u32,
    /// Foreground color (packed).
    pub fg: u32,
    /// Background color (packed).
    pub bg: u32,
    /// Underline color (packed). `UNDERLINE_USE_FG` means use foreground color.
    pub underline_color: u32,
    /// Cell flags (bold, italic, etc.).
    /// Bits 0-10: Standard visual attributes
    /// Bit 11: Superscript (SGR 73)
    /// Bit 12: Subscript (SGR 74)
    /// Bit 13: Curly underline
    /// Bit 14: USES_STYLE_ID — fg/bg store a StyleId, not colors
    /// Bit 15: COMPLEX — codepoint is an overflow table index
    pub flags: u16,
}

impl Default for AtermCell {
    fn default() -> Self {
        Self::CLEARED
    }
}

impl From<&Cell> for AtermCell {
    fn from(cell: &Cell) -> Self {
        // For complex cells (emoji, non-BMP chars), we can't know the actual
        // codepoint without extras lookup. Return 0xFFFD as placeholder.
        // Callers needing accurate codepoints should use from_cell_with_extra
        // or the aterm_cell_codepoint FFI function.
        let codepoint = if cell.is_complex() {
            0xFFFD
        } else {
            cell.char() as u32
        };

        // StyleId cells store a style table index in colors, not inline colors.
        // Preserve the raw packed value so consumers can extract the StyleId
        // (FLAG_USES_STYLE_ID in flags signals this encoding).
        // For inline colors, use fg_color()/bg_color(). RGB cells return None
        // here (placeholder black) — use from_cell_with_extra for full fidelity.
        let (fg, bg) = if cell.uses_style_id() {
            let raw = cell.colors().0;
            (raw, raw)
        } else {
            let fg = cell.fg_color().map_or(PackedColor::rgb(0, 0, 0).0, |c| c.0);
            let bg = cell.bg_color().map_or(PackedColor::rgb(0, 0, 0).0, |c| c.0);
            (fg, bg)
        };

        Self {
            codepoint,
            fg,
            bg,
            underline_color: Self::UNDERLINE_USE_FG,
            flags: cell.flags().bits(),
        }
    }
}

#[allow(
    dead_code,
    reason = "complete C ABI flag mirror + constructors consumed by the test/FFI/GPU layer"
)]
impl AtermCell {
    // =========================================================================
    // Sentinel values and cleared-output constructor.
    // =========================================================================

    /// Underline color sentinel: "use foreground color" (no custom underline).
    ///
    /// Matches the Swift-side contract (`ATermCell.swift:44`) and the
    /// behavioral contract tests (`contract_behavioral_tests.rs:247`).
    pub const UNDERLINE_USE_FG: u32 = 0xFFFF_FFFF;

    /// Cleared output cell — the canonical pre-clear value for error paths.
    ///
    /// All FFI single-cell getters write this to `out_cell` before any early
    /// return so callers never observe stale data.  `Default` delegates here.
    ///
    /// Uses default-type color encoding (`0xFF` type byte) so cleared cells
    /// render with the theme background/foreground instead of indexed black.
    pub const CLEARED: Self = Self {
        codepoint: 0,
        fg: PackedColor::DEFAULT_FG.0,
        bg: PackedColor::DEFAULT_BG.0,
        underline_color: Self::UNDERLINE_USE_FG,
        flags: 0,
    };

    // =========================================================================
    // CellFlags symbolic constants for C consumers.
    //
    // These mirror `CellFlags` in cell_flags.rs. Consumers should use these
    // instead of hardcoded bit positions when interpreting AtermCell.flags.
    // =========================================================================

    /// Bold text (bit 0).
    pub const FLAG_BOLD: u16 = 1 << 0;
    /// Dim/faint text (bit 1).
    pub const FLAG_DIM: u16 = 1 << 1;
    /// Italic text (bit 2).
    pub const FLAG_ITALIC: u16 = 1 << 2;
    /// Underlined text (bit 3).
    pub const FLAG_UNDERLINE: u16 = 1 << 3;
    /// Blinking text (bit 4).
    pub const FLAG_BLINK: u16 = 1 << 4;
    /// Inverse video (bit 5).
    pub const FLAG_INVERSE: u16 = 1 << 5;
    /// Hidden/invisible text (bit 6).
    pub const FLAG_HIDDEN: u16 = 1 << 6;
    /// Strikethrough text (bit 7).
    pub const FLAG_STRIKETHROUGH: u16 = 1 << 7;
    /// Double underline (bit 8).
    pub const FLAG_DOUBLE_UNDERLINE: u16 = 1 << 8;
    /// Wide character — occupies 2 cells (bit 9).
    pub const FLAG_WIDE: u16 = 1 << 9;
    /// Wide continuation / Protected — shared bit, mutually exclusive (bit 10).
    pub const FLAG_WIDE_CONTINUATION: u16 = 1 << 10;
    /// Superscript SGR 73 (bit 11).
    pub const FLAG_SUPERSCRIPT: u16 = 1 << 11;
    /// Subscript SGR 74 (bit 12).
    pub const FLAG_SUBSCRIPT: u16 = 1 << 12;
    /// Overline SGR 53 (bit 11 + bit 12 combo, mutually exclusive with super/subscript).
    pub const FLAG_OVERLINE: u16 = (1 << 11) | (1 << 12);
    /// Curly underline (bit 13).
    pub const FLAG_CURLY_UNDERLINE: u16 = 1 << 13;
    /// Cell uses StyleId instead of inline colors (bit 14).
    ///
    /// When set, `fg`/`bg` fields contain a packed StyleId, not color data.
    /// Use `aterm_cell_resolve_style_v2()` to get resolved colors.
    pub const FLAG_USES_STYLE_ID: u16 = 1 << 14;
    /// Complex character — codepoint is an overflow table index (bit 15).
    pub const FLAG_COMPLEX: u16 = 1 << 15;

    /// Create a AtermCell with optional extras from CellExtra.
    ///
    /// For complex cells (emoji, non-BMP chars), pass the `complex_char` from
    /// `extra.complex_char().and_then(|s| s.chars().next())`.
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "Cell is 8 bytes; &Cell matches FFI bridge API pattern"
    )]
    /// Widened from `pub(crate)` to `pub` for cross-crate FFI extraction (#2584).
    pub fn from_cell_with_extra(
        cell: &Cell,
        fg_rgb: Option<[u8; 3]>,
        bg_rgb: Option<[u8; 3]>,
        underline_color: Option<[u8; 3]>,
        extended_flags: u16,
        complex_char: Option<char>,
    ) -> Self {
        // Combine core flags from Cell with extended flags from CellExtra
        let flags = cell.flags().bits() | extended_flags;

        // Helper to convert RGB array to packed u32 (0x01_RRGGBB format)
        let rgb_to_packed = |[r, g, b]: [u8; 3]| {
            0x01_000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        };

        // StyleId cells store a style table index, not inline colors.
        // Preserve raw packed value; consumer checks FLAG_USES_STYLE_ID and
        // calls aterm_cell_resolve_style_v2() to get resolved colors.
        let (fg, bg) = if cell.uses_style_id() {
            let raw = cell.colors().0;
            (raw, raw)
        } else {
            // Get foreground color - use RGB from extras if available.
            // When cell is RGB-marked but extras are missing, fall back to terminal
            // default rather than the cell's placeholder (which is black RGB 0x01_000000).
            // This prevents #5598: RGB overflow loss producing black instead of default fg.
            let fg = if cell.fg_needs_overflow() {
                fg_rgb
                    .map(rgb_to_packed)
                    .unwrap_or(PackedColor::DEFAULT_FG.0)
            } else {
                cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0)
            };

            // Get background color - same safe fallback as foreground.
            let bg = if cell.bg_needs_overflow() {
                bg_rgb
                    .map(rgb_to_packed)
                    .unwrap_or(PackedColor::DEFAULT_BG.0)
            } else {
                cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0)
            };
            (fg, bg)
        };

        // Convert underline color
        let underline_u32 = underline_color
            .map(rgb_to_packed)
            .unwrap_or(Self::UNDERLINE_USE_FG);

        // Get codepoint - use complex_char for complex cells (emoji, non-BMP chars)
        let codepoint = if cell.is_complex() {
            complex_char.map(|c| c as u32).unwrap_or(0xFFFD)
        } else {
            cell.char() as u32
        };

        Self {
            codepoint,
            fg,
            bg,
            underline_color: underline_u32,
            flags,
        }
    }
}

/// Resolved style information for a cell, returned by `aterm_cell_resolve_style_v2`.
///
/// Contains fully resolved fg/bg/underline colors regardless of whether the
/// cell uses inline colors or a StyleId. Consumers should use this instead of
/// reading `AtermCell.fg`/`bg` directly when `FLAG_USES_STYLE_ID` may be set.
#[allow(
    dead_code,
    reason = "FFI ABI surface consumed by the test/FFI/GPU layer"
)]
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AtermResolvedStyle {
    /// Resolved foreground color (red component).
    pub fg_r: u8,
    /// Resolved foreground color (green component).
    pub fg_g: u8,
    /// Resolved foreground color (blue component).
    pub fg_b: u8,
    /// Resolved background color (red component).
    pub bg_r: u8,
    /// Resolved background color (green component).
    pub bg_g: u8,
    /// Resolved background color (blue component).
    pub bg_b: u8,
    /// Resolved underline color (red component).
    pub ul_r: u8,
    /// Resolved underline color (green component).
    pub ul_g: u8,
    /// Resolved underline color (blue component).
    pub ul_b: u8,
    /// Whether a custom underline color is set (false = use foreground color).
    pub has_underline_color: bool,
    /// Whether the cell uses a StyleId (FLAG_USES_STYLE_ID was set).
    pub uses_style_id: bool,
    /// Cell flags with all style attributes merged (original flags + style attrs).
    pub flags: u16,
}

#[allow(
    dead_code,
    reason = "FFI ABI surface consumed by the test/FFI/GPU layer"
)]
impl AtermResolvedStyle {
    /// All-zero instance for safe initialization before error returns.
    pub const ZEROED: Self = Self {
        fg_r: 0,
        fg_g: 0,
        fg_b: 0,
        bg_r: 0,
        bg_g: 0,
        bg_b: 0,
        ul_r: 0,
        ul_g: 0,
        ul_b: 0,
        has_underline_color: false,
        uses_style_id: false,
        flags: 0,
    };
}
