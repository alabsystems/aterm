// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic work counters for the renderer's per-frame claims.

use std::cell::Cell;

thread_local! {
    /// Window cell geometries derived off the primary face (what
    /// `Renderer::cell_geometry` does when it has none kept for the px): a
    /// metrics read, and a face parse for an instantiated variable primary.
    static CELL_GEOMETRY_DERIVATIONS: Cell<usize> = const { Cell::new(0) };
    /// Font files whose every mapped code point the script supplement's
    /// candidate pass classified by script (its per-file census).
    static SCRIPT_CENSUSES: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_cell_geometry_derivation() {
    CELL_GEOMETRY_DERIVATIONS.with(|n| n.set(n.get() + 1));
}

/// This thread's cell geometry derivations since the last take, zeroed.
pub fn take_cell_geometry_derivations() -> usize {
    CELL_GEOMETRY_DERIVATIONS.with(|n| n.replace(0))
}

pub(crate) fn count_script_census() {
    SCRIPT_CENSUSES.with(|n| n.set(n.get() + 1));
}

/// This thread's script-supplement censuses since the last take, zeroed.
pub fn take_script_censuses() -> usize {
    SCRIPT_CENSUSES.with(|n| n.replace(0))
}

thread_local! {
    /// Arts built by the registry's builders (`fx::indexed`, `fx::pieces`,
    /// `fx::rgba`), interned or not: the work a producer's memo spares.
    static ART_BUILDS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_art_build() {
    ART_BUILDS.with(|n| n.set(n.get() + 1));
}

/// This thread's art builds since the last take, zeroed.
pub fn take_art_builds() -> usize {
    ART_BUILDS.with(|n| n.replace(0))
}

thread_local! {
    /// Sprite column tables the fx rasterizer built (`fx_raster::SpriteCols`):
    /// the work its key and its laziness spare.
    static SPRITE_COLS_BUILDS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_sprite_cols_build() {
    SPRITE_COLS_BUILDS.with(|n| n.set(n.get() + 1));
}

/// This thread's sprite column-table builds since the last take, zeroed.
pub fn take_sprite_cols_builds() -> usize {
    SPRITE_COLS_BUILDS.with(|n| n.replace(0))
}

thread_local! {
    /// Frame art budget rulings that walked their layers (`FxBudget::rule`):
    /// the work its memo spares.
    static BUDGET_RULINGS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_budget_ruling() {
    BUDGET_RULINGS.with(|n| n.set(n.get() + 1));
}

/// This thread's walked budget rulings since the last take, zeroed.
pub fn take_budget_rulings() -> usize {
    BUDGET_RULINGS.with(|n| n.replace(0))
}
