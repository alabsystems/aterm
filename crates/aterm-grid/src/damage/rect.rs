// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Damage geometry types for rendering.

/// Line damage bounds for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineDamageBounds {
    /// Row index.
    pub line: u16,
    /// Left column (inclusive).
    pub left: u16,
    /// Right column (exclusive).
    pub right: u16,
}

impl LineDamageBounds {
    /// Create new line damage bounds.
    #[inline]
    #[cfg(test)]
    pub const fn new(line: u16, left: u16, right: u16) -> Self {
        Self { line, left, right }
    }

    /// Check if this bounds is empty (no damage).
    #[inline]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.left >= self.right
    }
}
