// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic work counters for the renderer's per-frame claims.

use std::cell::Cell;

thread_local! {
    static CELL_GEOMETRY_DERIVATIONS: Cell<usize> = const { Cell::new(0) };
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
    static BUDGET_RULINGS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_budget_ruling() {
    BUDGET_RULINGS.with(|n| n.set(n.get() + 1));
}

/// This thread's walked budget rulings since the last take, zeroed.
pub fn take_budget_rulings() -> usize {
    BUDGET_RULINGS.with(|n| n.replace(0))
}

thread_local! {
    static WEDGE_SLABS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_wedge_slab() {
    WEDGE_SLABS.with(|n| n.set(n.get() + 1));
}

/// This thread's ribbon slabs laid as wedges ([`crate::ribbon_wedge_into`])
/// since the last take, zeroed.
pub fn take_wedge_slabs() -> usize {
    WEDGE_SLABS.with(|n| n.replace(0))
}

thread_local! {
    static GLASS_SLABS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn count_glass_slab() {
    GLASS_SLABS.with(|n| n.set(n.get() + 1));
}

/// This thread's ribbon slabs laid as the light glass ([`crate::ribbon_glass_into`]) since the
/// last take, zeroed.
pub fn take_glass_slabs() -> usize {
    GLASS_SLABS.with(|n| n.replace(0))
}
