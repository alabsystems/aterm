// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid state pods: presentation, span anchors and cursor/region state.

mod anchors;
mod cursor;
mod presentation;

#[cfg(any(test, debug_assertions))]
pub use anchors::with_knob;
pub use anchors::{Anchor, AnchorId, AnchorRole, AnchorSet, FenceKnob, MAX_ANCHORS, knob};
pub use cursor::GridCursorState;
pub use presentation::{
    AbsoluteRowUpdate, BandSet, GridPresentationState, MAX_SELECTION_DAMAGE_BANDS, OutputOrigin,
    ROW_BAND_MOVES_PER_BATCH, ResizeShape, RowBandMove, RowBandMoves, SelectionDamage,
};
