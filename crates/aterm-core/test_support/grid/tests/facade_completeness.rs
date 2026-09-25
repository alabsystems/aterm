// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-time checks that `aterm_core::grid` still re-exports the extracted
//! `aterm-grid` types that cross-crate consumers rely on.
//!
//! Each line coerces `identity::<aterm_grid::T>` to `fn(aterm_grid::T) ->
//! grid::T`, which type-checks only while the two names are the same type,
//! so a broken or diverging re-export fails the test build. (This was sixteen
//! `#[test]` fns whose bodies could not fail at run time.)

use crate::grid;
use core::convert::identity;

const _: () = {
    let _: fn(aterm_grid::Cell) -> grid::Cell = identity;
    let _: fn(aterm_grid::CellFlags) -> grid::CellFlags = identity;
    let _: fn(aterm_grid::PackedColor) -> grid::PackedColor = identity;
    let _: fn(aterm_grid::PackedColors) -> grid::PackedColors = identity;
    let _: fn(aterm_grid::Damage) -> grid::Damage = identity;
    let _: fn(aterm_grid::PageStore) -> grid::PageStore = identity;
    let _: fn(aterm_grid::LineSize) -> grid::LineSize = identity;
    let _: fn(aterm_grid::RowFlags) -> grid::RowFlags = identity;
    let _: fn(aterm_grid::StyleId) -> grid::StyleId = identity;
    let _: fn(aterm_grid::Style) -> grid::Style = identity;
    let _: fn(aterm_grid::Color) -> grid::Color = identity;
    let _: fn(aterm_grid::StyleAttrs) -> grid::StyleAttrs = identity;
    let _: fn(aterm_grid::StyleTable) -> grid::StyleTable = identity;
    let _: fn(aterm_grid::Row) -> grid::Row = identity;
    let _: fn(aterm_grid::style::ExtendedStyleInfo) -> grid::style::ExtendedStyleInfo = identity;
    // Page and PageSlice are intentionally NOT re-exported (#5573); only
    // PageStore and PAGE_SIZE are part of the public facade.
    let _: usize = grid::page::PAGE_SIZE;
};
