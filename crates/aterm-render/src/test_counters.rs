// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic WORK counters for the renderer's per-frame claims (the
//! `aterm_grid::test_counters` idiom): a cost asserted as a count, never a
//! time. Each sits at the site that does the work, not the one that decides
//! to skip it. Compiled in test and debug builds, so a downstream crate's
//! tests can read them; never in a release build. Thread-local, so parallel
//! tests cannot bleed into each other.

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
