// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! [`VisibleCellsRepr`]: the `CheckpointMeta` form of a grid's [`VisibleCells`].
//!
//! Version skew, both directions. A producer that predates the keys writes none: they default to empty and the rows restore as every build before restored them.
//! A consumer that predates them ignores the keys, as `CheckpointMeta` ignores every key it does not know, and restores the grid blobs, which are byte for byte what it writes itself.
//! A value that does not read costs only these facts ([`lenient_visible_cells`]), and one that reads but does not fit the grid is skipped entry by entry (`Grid::restore_visible_cells`).

use crate::grid::VisibleCells;
use crate::grid::row::LineSize;

/// [`VisibleCells`] as `CheckpointMeta` writes it: integer tuples, each list left out when empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VisibleCellsRepr {
    /// `[row, size]` for each row that is not single width, the size spelled as the digit of the `ESC #` sequence that sets it: `6` DECDWL, `3` and `4` the top and bottom halves of DECDHL. Any other size is skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_sizes: Vec<(u16, u8)>,
    /// `[row, start, end]`: each half-open run of DECSCA-protected columns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected: Vec<(u16, u16, u16)>,
}

impl VisibleCellsRepr {
    /// Nothing carried, so the key is left out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.line_sizes.is_empty() && self.protected.is_empty()
    }

    /// The wire form of `cells`.
    #[must_use]
    pub fn from_cells(cells: &VisibleCells) -> Self {
        Self {
            line_sizes: cells
                .line_sizes
                .iter()
                .filter_map(|&(row, size)| {
                    let digit = match size {
                        LineSize::DoubleWidth => 6,
                        LineSize::DoubleHeightTop => 3,
                        LineSize::DoubleHeightBottom => 4,
                        _ => return None,
                    };
                    Some((row, digit))
                })
                .collect(),
            protected: cells.protected.clone(),
        }
    }

    /// The facts this wire form names, a line size it does not know skipped.
    #[must_use]
    pub fn into_cells(self) -> VisibleCells {
        VisibleCells {
            line_sizes: self
                .line_sizes
                .into_iter()
                .filter_map(|(row, digit)| {
                    let size = match digit {
                        6 => LineSize::DoubleWidth,
                        3 => LineSize::DoubleHeightTop,
                        4 => LineSize::DoubleHeightBottom,
                        _ => return None,
                    };
                    Some((row, size))
                })
                .collect(),
            protected: self.protected,
        }
    }
}

/// Read a carried [`VisibleCellsRepr`], or an empty one when the value is not one.
///
/// `CheckpointMeta` is parsed whole, so a strict field that failed would fail it and adopt the session onto a blank, repainted screen; these facts only refine a screen the grid blobs already carry.
pub(super) fn lenient_visible_cells<'de, D>(deserializer: D) -> Result<VisibleCellsRepr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Cells(VisibleCellsRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Cells(cells) => cells,
            Wire::Unreadable(_) => VisibleCellsRepr::default(),
        },
    )
}
