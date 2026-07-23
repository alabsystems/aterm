// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-time checks that `aterm_core::grid` still re-exports the extracted
//! `aterm-grid` types that cross-crate consumers rely on.

use crate::grid;

macro_rules! assert_type_identity {
    ($test_name:ident, $core_ty:ty, $grid_expr:expr_2021) => {
        #[test]
        fn $test_name() {
            let _: $core_ty = $grid_expr;
        }
    };
}

assert_type_identity!(cell_type_identity, grid::Cell, aterm_grid::Cell::default());
assert_type_identity!(
    cell_flags_type_identity,
    grid::CellFlags,
    aterm_grid::CellFlags::empty()
);
assert_type_identity!(
    packed_color_type_identity,
    grid::PackedColor,
    aterm_grid::PackedColor::DEFAULT_FG
);
assert_type_identity!(
    packed_colors_type_identity,
    grid::PackedColors,
    aterm_grid::PackedColors::DEFAULT
);
assert_type_identity!(
    damage_type_identity,
    grid::Damage,
    aterm_grid::Damage::default()
);
assert_type_identity!(
    page_store_type_identity,
    grid::PageStore,
    aterm_grid::PageStore::new()
);
assert_type_identity!(
    line_size_type_identity,
    grid::LineSize,
    aterm_grid::LineSize::SingleWidth
);
assert_type_identity!(
    row_flags_type_identity,
    grid::RowFlags,
    aterm_grid::RowFlags::empty()
);
assert_type_identity!(
    style_id_type_identity,
    grid::StyleId,
    aterm_grid::StyleId::default()
);
assert_type_identity!(
    style_type_identity,
    grid::Style,
    aterm_grid::Style::default()
);
assert_type_identity!(
    color_type_identity,
    grid::Color,
    aterm_grid::Color::DEFAULT_FG
);
assert_type_identity!(
    style_attrs_type_identity,
    grid::StyleAttrs,
    aterm_grid::StyleAttrs::empty()
);
assert_type_identity!(
    style_table_type_identity,
    grid::StyleTable,
    aterm_grid::StyleTable::new()
);

#[test]
fn row_type_identity() {
    let mut pages = aterm_grid::PageStore::new();
    // SAFETY: The test-local page store outlives the constructed row.
    let _: grid::Row = unsafe { aterm_grid::Row::new(8, &mut pages) };
}

// Page and PageSlice are intentionally NOT re-exported (#5573).
// Only PageStore and PAGE_SIZE are part of the public facade.
#[test]
fn page_size_accessible_via_facade() {
    let _: usize = grid::page::PAGE_SIZE;
}

#[test]
fn extended_style_info_type_identity() {
    fn accepts_core_extended(_: grid::style::ExtendedStyleInfo) {}

    let _: fn(aterm_grid::style::ExtendedStyleInfo) = accepts_core_extended;
}
