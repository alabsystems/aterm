// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Page-backed `Row` lifetime safety.
//!
//! `Row::new(cols, &mut PageStore)` and `Row::resize` are `pub unsafe fn`: the
//! backing `PageStore` must outlive every row allocated from it.
//! `GridStorage.rows` is `pub(crate)`, so external code cannot extract owned
//! rows that outlive their store.
//!
//! These tests check the correct `unsafe` pattern at runtime (Miri-exercisable)
//! and pin at compile time, at the bottom of this file, that both functions
//! stay `unsafe fn`. The compiler rejects the unsound uses:
//!
//! ```text
//! // COMPILE ERROR: call to unsafe function requires unsafe block
//! let mut pages = aterm_grid::PageStore::new();
//! let row = aterm_grid::Row::new(8, &mut pages);  // ERROR
//! ```
//!
//! ```text
//! // COMPILE ERROR: field `rows` of `GridStorage` is private
//! let grid_storage: aterm_grid::state::GridStorage = /* ... */;
//! let owned_row = grid_storage.rows.remove(0);  // ERROR
//! ```

use aterm_grid::{CellFlags, PackedColor, PageStore, Row};

// Correct unsafe usage: PageStore outlives rows

/// A row created through the `unsafe` boundary works while `pages` outlives it
/// (reverse declaration order drops `row` first).
#[test]
fn row_new_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` (same scope, reverse drop order).
    let row = unsafe { Row::new(80, &mut pages) };

    assert_eq!(row.cols(), 80);
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
}

/// Resizing a row through the `unsafe` boundary works; `pages` outlives it.
#[test]
fn row_resize_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(40, &mut pages) };

    // Write content before resize
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..40u16 {
        row.write_char_styled(col, 'A', fg, bg, CellFlags::empty());
    }
    assert_eq!(row.len(), 40);

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);

    // Original content survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'A', "col {col} content lost");
    }

    // New cells are empty
    for col in 40..80u16 {
        assert!(row.get(col).unwrap().is_empty(), "col {col} not empty");
    }
}

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

    // Content within the new bounds survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'B', "col {col} content lost");
    }
}

// Multiple rows sharing a PageStore: cross-row isolation

/// Many rows allocated from one PageStore do not corrupt each other.
#[test]
fn multiple_rows_cross_isolation() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows are dropped before `pages` (Vec dropped before local `pages`).
    let mut rows: Vec<Row> = (0..20)
        .map(|_| unsafe { Row::new(80, &mut pages) })
        .collect();

    // Write unique content to each row
    for (i, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            row.write_char_styled(col, c, fg, bg, CellFlags::empty());
        }
    }

    // Verify each row's content is intact — no cross-row aliasing
    for (i, row) in rows.iter().enumerate() {
        let expected = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            assert_eq!(
                row.get(col).unwrap().char(),
                expected,
                "row[{i}][{col}] corrupted"
            );
        }
    }
}

/// Resize some rows while others remain unchanged. Verify that resizing one
/// row doesn't corrupt another row's backing storage.
#[test]
fn resize_one_row_doesnt_corrupt_others() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows and `pages` live in the same scope.
    let mut row_a = unsafe { Row::new(40, &mut pages) };
    let mut row_b = unsafe { Row::new(40, &mut pages) };

    // Write distinct content
    for col in 0..40u16 {
        row_a.write_char_styled(col, 'X', fg, bg, CellFlags::empty());
        row_b.write_char_styled(col, 'Y', fg, bg, CellFlags::empty());
    }

    // Resize row_a — allocates new PageSlice, abandons old one
    // SAFETY: `pages` outlives both rows.
    unsafe { row_a.resize(120, &mut pages) };

    // row_b must be unaffected
    for col in 0..40u16 {
        assert_eq!(
            row_b.get(col).unwrap().char(),
            'Y',
            "row_b[{col}] corrupted after row_a resize"
        );
    }

    // row_a original content survived resize
    for col in 0..40u16 {
        assert_eq!(
            row_a.get(col).unwrap().char(),
            'X',
            "row_a[{col}] lost after resize"
        );
    }
}

// Compile-time pins, checked by rustc when this test target builds against
// the real `aterm_grid`: a regression is a build failure.

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

fn assert_row_new_is_unsafe<T: RowNewIsUnsafe<A>, A>(_: &T) {}
fn assert_row_resize_is_unsafe<T: RowResizeIsUnsafe<A>, A>(_: &T) {}

/// `Row::new` and `Row::resize` are not callable from safe code. The proof is
/// that this function compiles (see [`RowNewIsUnsafe`]).
#[test]
fn row_new_and_resize_stay_unsafe_fns() {
    assert_row_new_is_unsafe(&Row::new);
    assert_row_resize_is_unsafe(&Row::resize);
}
