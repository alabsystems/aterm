// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid FFI bindings.
//!
//! Provides C bindings for terminal grid manipulation.
//!
//! # Safety Pattern
//!
//! All FFI functions follow a standard null-guard-then-deref pattern:
//!
//! - **Pointer deref** (`unsafe { &*ptr }` / `unsafe { &mut *ptr }` / `unsafe { (*ptr).field }`):
//!   Pointer validated non-null above; caller guarantees validity per `# Safety` doc.
//! - **Output pointer writes** (`unsafe { *out_xxx = value }`):
//!   Guarded by `!ptr.is_null()` or validated in input checks.
//! - **Box management** (`Box::into_raw` / `Box::from_raw`):
//!   Ownership transfers to/from caller; pointer was created by `Box::into_raw`.

#[path = "ffi_bridge_types.rs"]
mod types;
// These FFI ABI types are constructed/used by the cfg(test) `ffi_impl`
// module below, the test_support FFI helpers, and the out-of-crate FFI
// extraction (aterm-core-ffi); they are dead only in the default lib build.
#[allow(
    unused_imports,
    reason = "FFI ABI types consumed by the test/FFI layer"
)]
pub use types::{AtermCell, AtermGrid, AtermResolvedStyle};

// =============================================================================
// V2 ERROR ENUM
// =============================================================================

// Grid errors consolidated into AtermTerminalError (Part of #4299).

// =============================================================================
// EXTERN "C" FUNCTIONS — test only
// =============================================================================
//
// Production extern "C" symbols have been extracted to aterm-core-ffi (#2584).
// Test builds within aterm-core still need these functions so that
// existing test helpers can call them.

#[cfg(test)]
#[allow(missing_docs, reason = "test FFI stubs — docs not required")]
mod ffi_impl {
    use super::*;
    use crate::grid::Grid;
    use crate::grid::extra::CellExtra;
    pub use aterm_ffi_types::AtermTerminalError;
    use aterm_ffi_types::ffi_ref_tracked;

    #[unsafe(no_mangle)]
    pub extern "C" fn aterm_grid_new(rows: u16, cols: u16) -> *mut AtermGrid {
        ffi_catch_panic!(std::ptr::null_mut(), "grid_new", {
            let ptr = Box::into_raw(Box::new(AtermGrid(Grid::new(rows, cols))));
            aterm_ffi_types::FfiTracker::Grid.mark_allocated(ptr.cast());
            ptr
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn aterm_grid_new_with_scrollback(
        rows: u16,
        cols: u16,
        max_scrollback: usize,
    ) -> *mut AtermGrid {
        ffi_catch_panic!(std::ptr::null_mut(), "grid_new_with_scrollback", {
            let ptr = Box::into_raw(Box::new(AtermGrid(Grid::with_scrollback(
                rows,
                cols,
                max_scrollback,
            ))));
            aterm_ffi_types::FfiTracker::Grid.mark_allocated(ptr.cast());
            ptr
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_free(grid: *mut AtermGrid) {
        // SAFETY: Caller guarantees grid is null or valid pointer from Box::into_raw.
        unsafe {
            aterm_ffi_types::box_handle_free_v1(
                "grid_free",
                grid,
                aterm_ffi_types::FfiTracker::Grid,
            );
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_rows(grid: *const AtermGrid) -> u16 {
        ffi_catch_panic!(0, "grid_rows", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.rows()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_cols(grid: *const AtermGrid) -> u16 {
        ffi_catch_panic!(0, "grid_cols", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.cols()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_cursor_row(grid: *const AtermGrid) -> u16 {
        ffi_catch_panic!(0, "grid_cursor_row", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.cursor_row()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_cursor_col(grid: *const AtermGrid) -> u16 {
        ffi_catch_panic!(0, "grid_cursor_col", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.cursor_col()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_get_cell_v2(
        grid: *const AtermGrid,
        row: u16,
        col: u16,
        out_cell: *mut AtermCell,
    ) -> AtermTerminalError {
        ffi_catch_panic!(AtermTerminalError::ErrInternal, "grid_get_cell_v2", {
            if !out_cell.is_null() {
                // SAFETY: Output pointer validated non-null; caller guarantees writable memory.
                unsafe { *out_cell = AtermCell::CLEARED };
            }

            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return AtermTerminalError::ErrNullTerminal;
            };

            if out_cell.is_null() {
                return AtermTerminalError::ErrNullOutput;
            }

            let grid_ref = &grid.0;

            match grid_ref.cell(row, col) {
                Some(cell) => {
                    let render = grid_ref.cell_render_data(row, col, *cell);
                    let fg_rgb = render.fg_rgb();
                    let bg_rgb = render.bg_rgb();
                    let extra = render.cell_extra();
                    let underline_color = extra.and_then(CellExtra::underline_color);
                    let extended_flags = extra.map(CellExtra::extended_flags).unwrap_or(0);
                    let complex_char = render.complex_char();
                    // SAFETY: Output pointer validated non-null; caller guarantees writable memory.
                    unsafe {
                        *out_cell = AtermCell::from_cell_with_extra(
                            cell,
                            fg_rgb,
                            bg_rgb,
                            underline_color,
                            extended_flags,
                            complex_char,
                        );
                    }
                    AtermTerminalError::Ok
                }
                None => AtermTerminalError::ErrOutOfBounds,
            }
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_display_offset(grid: *const AtermGrid) -> usize {
        ffi_catch_panic!(0, "grid_display_offset", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.display_offset()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_scrollback_lines(grid: *const AtermGrid) -> usize {
        ffi_catch_panic!(0, "grid_scrollback_lines", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.scrollback_lines()
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_needs_redraw(grid: *const AtermGrid) -> bool {
        ffi_catch_panic!(false, "grid_needs_redraw", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return false;
            };
            grid.0.needs_full_redraw()
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn aterm_is_box_drawing_character(codepoint: u32) -> bool {
        ffi_catch_panic!(false, "is_box_drawing_character", {
            if let Some(c) = char::from_u32(codepoint) {
                matches!(c,
                    '\u{2500}'..='\u{257F}' |  // Box Drawing
                    '\u{2580}'..='\u{259F}' |  // Block Elements
                    '\u{25E2}'..='\u{25FF}' |  // Geometric Shapes (triangles)
                    '\u{1FB00}'..='\u{1FB3B}'  // Legacy Terminal (sextants)
                )
            } else {
                false
            }
        })
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aterm_grid_visible_to_absolute(
        grid: *const AtermGrid,
        visible_row: u16,
    ) -> u64 {
        ffi_catch_panic!(0, "grid_visible_to_absolute", {
            let Some(grid) = (unsafe { ffi_ref_tracked(grid, aterm_ffi_types::FfiTracker::Grid) })
            else {
                return 0;
            };
            grid.0.visible_to_absolute(visible_row)
        })
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "test FFI symbol surface; individual symbols are exercised by select test helpers"
)]
pub use ffi_impl::{
    AtermTerminalError, aterm_grid_cols, aterm_grid_cursor_col, aterm_grid_cursor_row,
    aterm_grid_display_offset, aterm_grid_free, aterm_grid_get_cell_v2, aterm_grid_needs_redraw,
    aterm_grid_new, aterm_grid_new_with_scrollback, aterm_grid_rows, aterm_grid_scrollback_lines,
    aterm_grid_visible_to_absolute, aterm_is_box_drawing_character,
};

#[cfg(test)]
#[path = "../../test_support/grid/ffi_bridge_tests.rs"]
mod tests;
