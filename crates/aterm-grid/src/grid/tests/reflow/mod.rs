// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow tests — shrink/grow wrapping, cursor tracking, wide chars, protected cells.

mod core;
mod cost_bound;
mod cursor_invariants;
mod decdwl;
mod performance;
mod protected_cells;
mod regressions;
mod rows_only_ring_retention;
mod wide_chars;
mod width_sweep;
