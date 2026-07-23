// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for Grid FFI bindings.

use super::*;
use crate::grid::PackedColor;
use std::ptr;

/// All-zero `AtermCell` out-parameter for `aterm_grid_get_cell_v2` tests.
/// Previously provided by the now-removed in-crate `ffi::test_helpers`.
fn zeroed_test_cell() -> AtermCell {
    AtermCell {
        codepoint: 0,
        fg: 0,
        bg: 0,
        underline_color: 0,
        flags: 0,
    }
}

macro_rules! ffi_call {
    ($expr:expr_2021 $(,)?) => {{
        // SAFETY: Tests intentionally call FFI entry points with controlled
        // pointers to verify null/bounds behavior.
        unsafe { $expr }
    }};
}

fn grid_get_cell_v2_raw(
    grid: *const AtermGrid,
    row: u16,
    col: u16,
    out_cell: *mut AtermCell,
) -> AtermTerminalError {
    ffi_call!(aterm_grid_get_cell_v2(grid, row, col, out_cell))
}

fn grid_write_char_via_inner(grid: *mut AtermGrid, codepoint: u32) {
    // SAFETY: Grid pointer validated by test setup (non-null GridHandle).
    if let Some(ch) = char::from_u32(codepoint) {
        unsafe { (*grid).0.write_char(ch) };
    }
}

fn grid_free_raw(grid: *mut AtermGrid) {
    ffi_call!(aterm_grid_free(grid));
}

struct GridHandle {
    raw: *mut AtermGrid,
}

impl GridHandle {
    fn new(rows: u16, cols: u16) -> Self {
        let raw = aterm_grid_new(rows, cols);
        assert!(!raw.is_null(), "grid should not be null");
        Self { raw }
    }

    fn as_const_ptr(&self) -> *const AtermGrid {
        self.raw.cast_const()
    }

    fn get_cell_v2_raw(&self, row: u16, col: u16, out_cell: *mut AtermCell) -> AtermTerminalError {
        grid_get_cell_v2_raw(self.as_const_ptr(), row, col, out_cell)
    }

    fn write_char_raw(&mut self, codepoint: u32) {
        grid_write_char_via_inner(self.raw, codepoint);
    }
}

impl Drop for GridHandle {
    fn drop(&mut self) {
        grid_free_raw(self.raw);
    }
}

#[test]
fn test_get_cell_v2_success() {
    let grid = GridHandle::new(10, 20);
    let mut cell = zeroed_test_cell();
    let result = grid.get_cell_v2_raw(0, 0, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::Ok,
        "get_cell_v2 should succeed for valid inputs"
    );

    // Verify cell content — empty grid cell should be a space with default colors
    assert_eq!(
        cell.codepoint, ' ' as u32,
        "empty cell codepoint should be space (0x20), got 0x{:X}",
        cell.codepoint
    );
    assert_eq!(cell.flags, 0, "empty cell should have no flags set");
}

#[test]
fn test_get_cell_v2_null_grid() {
    let mut cell = zeroed_test_cell();
    let result = grid_get_cell_v2_raw(ptr::null(), 0, 0, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::ErrNullTerminal,
        "get_cell_v2 should return ErrNullTerminal for null grid"
    );
}

#[test]
fn test_get_cell_v2_null_output() {
    let grid = GridHandle::new(10, 20);
    let result = grid.get_cell_v2_raw(0, 0, ptr::null_mut());
    assert_eq!(
        result,
        AtermTerminalError::ErrNullOutput,
        "get_cell_v2 should return ErrNullOutput for null output"
    );
}

#[test]
fn test_get_cell_v2_out_of_bounds_row() {
    let grid = GridHandle::new(10, 20);
    let mut cell = zeroed_test_cell();
    // Row 10 is out of bounds (valid range is 0..10)
    let result = grid.get_cell_v2_raw(10, 0, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::ErrOutOfBounds,
        "get_cell_v2 should return ErrOutOfBounds for row 10"
    );
}

#[test]
fn test_get_cell_v2_out_of_bounds_col() {
    let grid = GridHandle::new(10, 20);
    let mut cell = zeroed_test_cell();
    // Column 20 is out of bounds (valid range is 0..20)
    let result = grid.get_cell_v2_raw(0, 20, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::ErrOutOfBounds,
        "get_cell_v2 should return ErrOutOfBounds for col 20"
    );
}

#[test]
fn test_get_cell_v2_both_null() {
    // When both grid and output are null, grid is checked first
    let result = grid_get_cell_v2_raw(ptr::null(), 0, 0, ptr::null_mut());
    assert_eq!(
        result,
        AtermTerminalError::ErrNullTerminal,
        "get_cell_v2 should return ErrNullTerminal when both args are null"
    );
}

#[test]
fn test_get_cell_v2_last_valid_cell() {
    let grid = GridHandle::new(10, 20);
    let mut cell = zeroed_test_cell();
    // Last valid cell is at (9, 19)
    let result = grid.get_cell_v2_raw(9, 19, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::Ok,
        "get_cell_v2 should succeed for last valid cell (9, 19)"
    );

    // Verify the cell has valid content (empty cell = space)
    assert_eq!(
        cell.codepoint, ' ' as u32,
        "last valid cell should be space, got 0x{:X}",
        cell.codepoint
    );
    assert_eq!(cell.flags, 0, "last valid cell should have no flags set");
}

#[test]
fn test_get_cell_v2_output_cleared_on_error() {
    let grid = GridHandle::new(10, 20);
    let mut cell = AtermCell {
        codepoint: 0xDEAD,
        fg: 0xBEEF,
        bg: 0xCAFE,
        underline_color: 0xF00D,
        flags: 0xFF,
    };

    // Out of bounds should clear the output to AtermCell::CLEARED
    let result = grid.get_cell_v2_raw(100, 0, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::ErrOutOfBounds,
        "get_cell_v2 should return ErrOutOfBounds"
    );
    assert_eq!(cell.codepoint, 0, "codepoint should be cleared on error");
    assert_eq!(
        cell.fg,
        PackedColor::DEFAULT_FG.0,
        "fg should be default foreground on error"
    );
    assert_eq!(
        cell.bg,
        PackedColor::DEFAULT_BG.0,
        "bg should be default background on error"
    );
    assert_eq!(
        cell.underline_color,
        AtermCell::UNDERLINE_USE_FG,
        "underline_color should be UNDERLINE_USE_FG on error"
    );
    assert_eq!(cell.flags, 0, "flags should be cleared on error");
}

#[test]
fn test_get_cell_v2_output_cleared_on_null_grid() {
    let mut cell = AtermCell {
        codepoint: 0xDEAD,
        fg: 0xBEEF,
        bg: 0xCAFE,
        underline_color: 0xF00D,
        flags: 0xFF,
    };

    // Null grid should still clear the output to AtermCell::CLEARED
    let result = grid_get_cell_v2_raw(ptr::null(), 0, 0, &mut cell);
    assert_eq!(
        result,
        AtermTerminalError::ErrNullTerminal,
        "get_cell_v2 should return ErrNullTerminal"
    );
    assert_eq!(
        cell.codepoint, 0,
        "codepoint should be cleared on null grid"
    );
    assert_eq!(
        cell.fg,
        PackedColor::DEFAULT_FG.0,
        "fg should be default foreground on null grid"
    );
    assert_eq!(
        cell.bg,
        PackedColor::DEFAULT_BG.0,
        "bg should be default background on null grid"
    );
    assert_eq!(
        cell.underline_color,
        AtermCell::UNDERLINE_USE_FG,
        "underline_color should be UNDERLINE_USE_FG on null grid"
    );
    assert_eq!(cell.flags, 0, "flags should be cleared on null grid");
}

/// Regression guard for #6704: AtermCell::CLEARED and AtermCell::default()
/// must use default-type color encoding (type byte 0xFF), not indexed-black
/// (type byte 0x00). Zero-init bg=0 caused dark bands on light themes.
#[test]
fn test_cleared_cell_uses_default_type_encoding() {
    let cleared = AtermCell::CLEARED;
    let defaulted = AtermCell::default();

    // Type byte is bits 31..24. Default = 0xFF, indexed = 0x00.
    assert_eq!(
        cleared.bg >> 24,
        0xFF,
        "CLEARED.bg type byte must be 0xFF (default), got 0x{:02X}",
        cleared.bg >> 24,
    );
    assert_eq!(
        cleared.fg >> 24,
        0xFF,
        "CLEARED.fg type byte must be 0xFF (default), got 0x{:02X}",
        cleared.fg >> 24,
    );
    assert_eq!(
        cleared.bg, defaulted.bg,
        "CLEARED and default() must agree on bg"
    );
    assert_eq!(
        cleared.fg, defaulted.fg,
        "CLEARED and default() must agree on fg"
    );
}

#[test]
fn test_get_cell_v2_after_write_returns_correct_content() {
    let mut grid = GridHandle::new(10, 20);

    // Write 'A' at cursor position (0, 0)
    grid.write_char_raw('A' as u32);

    let mut cell = zeroed_test_cell();
    let result = grid.get_cell_v2_raw(0, 0, &mut cell);
    assert_eq!(result, AtermTerminalError::Ok);
    assert_eq!(
        cell.codepoint, 'A' as u32,
        "cell (0,0) should contain 'A' after write, got codepoint 0x{:X}",
        cell.codepoint
    );

    // Cell at (0, 1) should still be empty (space) — write should not bleed
    let mut cell2 = zeroed_test_cell();
    let result2 = grid.get_cell_v2_raw(0, 1, &mut cell2);
    assert_eq!(result2, AtermTerminalError::Ok);
    assert_eq!(
        cell2.codepoint, ' ' as u32,
        "cell (0,1) should still be space after writing to (0,0), got 0x{:X}",
        cell2.codepoint
    );
}

/// Test Legacy Terminal sextant range (U+1FB00-U+1FB3B)
#[test]
fn test_box_drawing_character_sextants() {
    // Before range
    assert!(
        !aterm_is_box_drawing_character(0x1FAFF),
        "0x1FAFF should not be box drawing"
    );
    // Range boundaries
    assert!(
        aterm_is_box_drawing_character(0x1FB00),
        "0x1FB00 should be box drawing (sextant start)"
    );
    assert!(
        aterm_is_box_drawing_character(0x1FB3B),
        "0x1FB3B should be box drawing (sextant end)"
    );
    // After range
    assert!(
        !aterm_is_box_drawing_character(0x1FB3C),
        "0x1FB3C should not be box drawing"
    );
    assert!(
        !aterm_is_box_drawing_character(0x1FB3D),
        "0x1FB3D should not be box drawing"
    );
}

/// Regression test for #5598: RGB-marked cells with missing extras must
/// return terminal defaults, not black placeholder (0x01_000000).
#[test]
fn test_from_cell_with_extra_rgb_missing_extras_returns_defaults() {
    use crate::grid::{Cell, CellFlags, PackedColor};

    // Create an RGB-marked cell. PackedColor::rgb() marks fg/bg as needing
    // overflow, but we deliberately pass None for fg_rgb/bg_rgb to simulate
    // the missing-extras condition.
    let cell = Cell::with_style(
        'X',
        PackedColor::rgb(128, 64, 32),
        PackedColor::rgb(32, 64, 128),
        CellFlags::empty(),
    );
    assert!(cell.fg_needs_overflow(), "cell should be RGB-marked for fg");
    assert!(cell.bg_needs_overflow(), "cell should be RGB-marked for bg");

    // Call from_cell_with_extra with no RGB data — simulates overflow loss
    let aterm_cell = AtermCell::from_cell_with_extra(
        &cell, None, // fg_rgb missing
        None, // bg_rgb missing
        None, // underline_color
        0,    // extended_flags
        None, // complex_char
    );

    assert_eq!(
        aterm_cell.fg,
        PackedColor::DEFAULT_FG.0,
        "RGB cell with missing fg extras should get DEFAULT_FG (0x{:08X}), got 0x{:08X}",
        PackedColor::DEFAULT_FG.0,
        aterm_cell.fg,
    );
    assert_eq!(
        aterm_cell.bg,
        PackedColor::DEFAULT_BG.0,
        "RGB cell with missing bg extras should get DEFAULT_BG (0x{:08X}), got 0x{:08X}",
        PackedColor::DEFAULT_BG.0,
        aterm_cell.bg,
    );
}

/// Verify from_cell_with_extra still works correctly when RGB extras ARE present.
#[test]
fn test_from_cell_with_extra_rgb_present_extras_returns_rgb() {
    use crate::grid::{Cell, CellFlags, PackedColor};

    let cell = Cell::with_style(
        'Y',
        PackedColor::rgb(200, 100, 50),
        PackedColor::rgb(50, 100, 200),
        CellFlags::empty(),
    );

    let aterm_cell = AtermCell::from_cell_with_extra(
        &cell,
        Some([200, 100, 50]),
        Some([50, 100, 200]),
        None,
        0,
        None,
    );

    // 0x01_RRGGBB format
    let expected_fg = 0x01_000000 | (200 << 16) | (100 << 8) | 50;
    let expected_bg = 0x01_000000 | (50 << 16) | (100 << 8) | 200;
    assert_eq!(
        aterm_cell.fg, expected_fg,
        "RGB cell with present extras should get actual RGB"
    );
    assert_eq!(
        aterm_cell.bg, expected_bg,
        "RGB cell with present extras should get actual RGB"
    );
}

/// Regression test for #5890 H5: StyleId cells must preserve the raw
/// StyleId bits in AtermCell.fg/bg, not return placeholder black (0x01_000000).
#[test]
fn test_from_style_id_cell_preserves_style_id_in_fg_bg() {
    use crate::grid::{Cell, CellFlags, StyleId};

    let style_id = StyleId::new(42);
    let cell = Cell::with_style_id('A', style_id, CellFlags::empty());
    assert!(cell.uses_style_id(), "cell should have USES_STYLE_ID flag");

    // From<&Cell> — simple conversion
    let aterm_cell = AtermCell::from(&cell);
    let raw = cell.colors().0;
    assert_ne!(
        raw, 0x01_000000,
        "raw colors should not be placeholder black"
    );
    assert_eq!(
        aterm_cell.fg, raw,
        "StyleId cell fg should be raw packed colors (0x{:08X}), got 0x{:08X}",
        raw, aterm_cell.fg,
    );
    assert_eq!(
        aterm_cell.bg, raw,
        "StyleId cell bg should be raw packed colors (0x{:08X}), got 0x{:08X}",
        raw, aterm_cell.bg,
    );
    assert_ne!(
        aterm_cell.flags & AtermCell::FLAG_USES_STYLE_ID,
        0,
        "AtermCell.flags should have FLAG_USES_STYLE_ID set"
    );
    // Verify the StyleId value is recoverable from the low 16 bits
    assert_eq!(
        aterm_cell.fg & 0xFFFF,
        42,
        "StyleId value (42) should be recoverable from fg low 16 bits"
    );
}

/// Regression test for #5890 H5: from_cell_with_extra also preserves StyleId.
#[test]
fn test_from_cell_with_extra_style_id_preserves_raw_colors() {
    use crate::grid::{Cell, CellFlags, StyleId};

    let style_id = StyleId::new(7);
    let cell = Cell::with_style_id('B', style_id, CellFlags::BOLD);
    assert!(cell.uses_style_id());

    let aterm_cell = AtermCell::from_cell_with_extra(
        &cell, None, // fg_rgb — irrelevant for StyleId cells
        None, // bg_rgb
        None, // underline_color
        0,    // extended_flags
        None, // complex_char
    );

    let raw = cell.colors().0;
    assert_eq!(
        aterm_cell.fg, raw,
        "from_cell_with_extra: StyleId cell fg should be raw packed, got 0x{:08X}",
        aterm_cell.fg,
    );
    assert_eq!(
        aterm_cell.bg, raw,
        "from_cell_with_extra: StyleId cell bg should be raw packed, got 0x{:08X}",
        aterm_cell.bg,
    );
    assert_eq!(
        aterm_cell.fg & 0xFFFF,
        7,
        "StyleId value (7) should be recoverable from fg"
    );
    // BOLD should be in flags (from cell_flags, not extended_flags)
    assert_ne!(
        aterm_cell.flags & AtermCell::FLAG_BOLD,
        0,
        "BOLD flag should be preserved"
    );
}

/// Verify that default-colored (non-StyleId, non-RGB) cells still work after
/// the migration from deprecated fg()/bg() to fg_color()/bg_color().
#[test]
fn test_from_cell_with_extra_default_colors_unchanged() {
    use crate::grid::{Cell, CellFlags, PackedColor};

    // Default-colored cell (no RGB, no StyleId)
    let cell = Cell::with_style(
        'Z',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert!(!cell.uses_style_id());
    assert!(!cell.fg_needs_overflow());
    assert!(!cell.bg_needs_overflow());

    let aterm_cell = AtermCell::from_cell_with_extra(&cell, None, None, None, 0, None);
    assert_eq!(
        aterm_cell.fg,
        PackedColor::DEFAULT_FG.0,
        "default fg should be DEFAULT_FG"
    );
    assert_eq!(
        aterm_cell.bg,
        PackedColor::DEFAULT_BG.0,
        "default bg should be DEFAULT_BG"
    );
}

/// Verify indexed colors pass through correctly after the accessor migration.
#[test]
fn test_from_cell_with_extra_indexed_colors_unchanged() {
    use crate::grid::{Cell, CellFlags, PackedColor};

    let cell = Cell::with_style(
        'C',
        PackedColor::indexed(196), // bright red
        PackedColor::indexed(21),  // blue
        CellFlags::ITALIC,
    );
    assert!(!cell.uses_style_id());
    assert!(!cell.fg_needs_overflow());

    let aterm_cell = AtermCell::from_cell_with_extra(&cell, None, None, None, 0, None);
    assert_eq!(
        aterm_cell.fg,
        PackedColor::indexed(196).0,
        "indexed fg should pass through"
    );
    assert_eq!(
        aterm_cell.bg,
        PackedColor::indexed(21).0,
        "indexed bg should pass through"
    );
}
