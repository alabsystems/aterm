// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Page-backed `Row` lifetime and `unsafe fn` signature checks.

use aterm_grid::{CellFlags, PackedColor, PageStore, Row};

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

const _: unsafe fn(u16, &mut PageStore) -> Row = Row::new;
const _: unsafe fn(&mut Row, u16, &mut PageStore) = Row::resize;

struct SafeFn;

trait RowNewIsUnsafe<A> {}
impl<T> RowNewIsUnsafe<()> for T {}
impl<T: Fn(u16, &mut PageStore) -> Row> RowNewIsUnsafe<SafeFn> for T {}

trait RowResizeIsUnsafe<A> {}
impl<T> RowResizeIsUnsafe<()> for T {}
impl<T: Fn(&mut Row, u16, &mut PageStore)> RowResizeIsUnsafe<SafeFn> for T {}

const fn assert_row_new_is_unsafe<T: RowNewIsUnsafe<A>, A>(_: &T) {}
const fn assert_row_resize_is_unsafe<T: RowResizeIsUnsafe<A>, A>(_: &T) {}

const _: () = {
    assert_row_new_is_unsafe(&Row::new);
    assert_row_resize_is_unsafe(&Row::resize);
};
