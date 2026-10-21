// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Page-backed `Row` lifetime safety.
//!
//! `Row::new(cols, &mut PageStore)` and `Row::resize` are `pub unsafe fn`: the backing
//! `PageStore` must outlive every row allocated from it. `Grid::storage` is `pub(crate)`, so
//! external code cannot extract a grid's rows to outlive their store.
//!
//! These tests check the correct `unsafe` pattern at runtime (Miri-exercisable),
//! and the items at the bottom of this file pin at compile time that both
//! functions stay `unsafe fn`. The compiler rejects the unsound uses:
//!
//! ```text
//! // COMPILE ERROR: call to unsafe function requires unsafe block
//! let mut pages = aterm_grid::PageStore::new();
//! let row = aterm_grid::Row::new(8, &mut pages);  // ERROR
//! ```

use aterm_grid::{CellFlags, PackedColor, PageStore, Row};

// Correct unsafe usage: PageStore outlives rows

/// Shrink-resize through the `unsafe` boundary works.
#[test]
fn row_resize_shrink_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(80, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..80u16 {
        row.write_char_styled(col, 'B', fg, bg, CellFlags::empty());
    }

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(40, &mut pages) };
    assert_eq!(row.cols(), 40);

    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'B', "col {col} content lost");
    }
}

// Multiple rows sharing a PageStore: cross-row isolation

/// Resize some rows while others remain unchanged. Verify that resizing one
/// row doesn't corrupt another row's backing storage.
#[test]
fn resize_one_row_doesnt_corrupt_others() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: `pages` is declared first, so it outlives both rows.
    let mut row_a = unsafe { Row::new(40, &mut pages) };
    let mut row_b = unsafe { Row::new(40, &mut pages) };

    for col in 0..40u16 {
        row_a.write_char_styled(col, 'X', fg, bg, CellFlags::empty());
        row_b.write_char_styled(col, 'Y', fg, bg, CellFlags::empty());
    }

    // Resizing row_a allocates a new PageSlice and abandons the old one.
    // SAFETY: `pages` outlives both rows.
    unsafe { row_a.resize(120, &mut pages) };

    for col in 0..40u16 {
        assert_eq!(
            row_b.get(col).unwrap().char(),
            'Y',
            "row_b[{col}] corrupted after row_a resize"
        );
    }

    for col in 0..40u16 {
        assert_eq!(
            row_a.get(col).unwrap().char(),
            'X',
            "row_a[{col}] lost after resize"
        );
    }
}

// Compile-time pins: a regression is a build failure of this test target.

/// Positive control and signature pin: `Row::new` and `Row::resize` coerce to
/// `unsafe fn` pointers of exactly these signatures, so the negative pins below
/// are about these functions.
const _: unsafe fn(u16, &mut PageStore) -> Row = Row::new;
const _: unsafe fn(&mut Row, u16, &mut PageStore) = Row::resize;

/// Marker for the "is a SAFE callable" impls below.
struct SafeFn;

/// Every type implements this through `()`; a type that is also a safe
/// `Fn(u16, &mut PageStore) -> Row` implements it through [`SafeFn`] too. An
/// `unsafe fn` implements no `Fn` trait, so `A` resolves to `()`; were
/// `Row::new` safe, the call below would be ambiguous (E0283). This is the
/// `assert_not_impl_any` technique.
trait RowNewIsUnsafe<A> {}
impl<T> RowNewIsUnsafe<()> for T {}
impl<T: Fn(u16, &mut PageStore) -> Row> RowNewIsUnsafe<SafeFn> for T {}

/// [`RowNewIsUnsafe`], for `Row::resize`.
trait RowResizeIsUnsafe<A> {}
impl<T> RowResizeIsUnsafe<()> for T {}
impl<T: Fn(&mut Row, u16, &mut PageStore)> RowResizeIsUnsafe<SafeFn> for T {}

const fn assert_row_new_is_unsafe<T: RowNewIsUnsafe<A>, A>(_: &T) {}
const fn assert_row_resize_is_unsafe<T: RowResizeIsUnsafe<A>, A>(_: &T) {}

/// `Row::new` and `Row::resize` are not callable from safe code. The proof is
/// that this item compiles (see [`RowNewIsUnsafe`]).
const _: () = {
    assert_row_new_is_unsafe(&Row::new);
    assert_row_resize_is_unsafe(&Row::resize);
};
