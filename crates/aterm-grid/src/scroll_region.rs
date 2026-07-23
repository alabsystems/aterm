// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scroll region and horizontal margin types for terminal grid.

/// Scroll region bounds (top and bottom, inclusive, 0-indexed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollRegion {
    /// Top row of scroll region (inclusive, 0-indexed).
    pub top: u16,
    /// Bottom row of scroll region (inclusive, 0-indexed).
    pub bottom: u16,
}

impl ScrollRegion {
    /// Create a scroll region covering all visible rows.
    #[inline]
    pub(crate) fn full(visible_rows: u16) -> Self {
        Self {
            top: 0,
            bottom: visible_rows.saturating_sub(1),
        }
    }

    /// Check if this is the full screen (no restricted region).
    #[inline]
    pub(crate) fn is_full(self, visible_rows: u16) -> bool {
        self.top == 0 && self.bottom == visible_rows.saturating_sub(1)
    }
}

/// Horizontal margin bounds for DECSLRM (left and right, inclusive, 0-indexed).
///
/// VT420+: Left/right margins restrict cursor movement and line operations
/// within the margin boundaries. Only active when DECLRMM (mode 69) is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizontalMargins {
    /// Left column margin (inclusive, 0-indexed).
    pub left: u16,
    /// Right column margin (inclusive, 0-indexed).
    pub right: u16,
}

impl HorizontalMargins {
    /// Create horizontal margins covering all columns.
    #[must_use]
    #[inline]
    pub fn full(cols: u16) -> Self {
        Self {
            left: 0,
            right: cols.saturating_sub(1),
        }
    }

    /// Check if this covers the full width (no restricted region).
    #[must_use]
    #[inline]
    pub fn is_full(self, cols: u16) -> bool {
        self.left == 0 && self.right == cols.saturating_sub(1)
    }
}
