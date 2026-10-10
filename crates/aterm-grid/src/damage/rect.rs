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
}

#[cfg(any(test, feature = "testing"))]
impl LineDamageBounds {
    /// Check if this bounds is empty (no damage).
    #[inline]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.left >= self.right
    }

    /// Whether two rows are consecutive with overlapping or adjacent column
    /// ranges, so they merge into one rectangle.
    #[inline]
    #[cfg(test)]
    pub fn can_merge_with(&self, other: &Self) -> bool {
        // Must be adjacent lines
        if self.line.abs_diff(other.line) != 1 {
            return false;
        }
        // Column ranges must overlap or be adjacent
        self.left <= other.right && other.left <= self.right
    }

    /// Merge with another bounds into a rectangle covering both (check
    /// `can_merge_with` first).
    #[inline]
    #[cfg(test)]
    pub fn merge_with(&self, other: &Self) -> DamageRect {
        DamageRect {
            top: self.line.min(other.line),
            bottom: self.line.max(other.line) + 1,
            left: self.left.min(other.left),
            right: self.right.max(other.right),
        }
    }
}

/// A rectangular damage region spanning several rows, batching adjacent
/// damaged rows into fewer GPU draw calls.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    /// Top row (inclusive).
    pub top: u16,
    /// Bottom row (exclusive).
    pub bottom: u16,
    /// Left column (inclusive).
    pub left: u16,
    /// Right column (exclusive).
    pub right: u16,
}

#[cfg(any(test, feature = "testing"))]
impl DamageRect {
    /// Create a new damage rectangle.
    #[inline]
    #[cfg(test)]
    pub const fn new(top: u16, bottom: u16, left: u16, right: u16) -> Self {
        Self {
            top,
            bottom,
            left,
            right,
        }
    }

    /// Create a rectangle from a single line bounds.
    #[inline]
    pub const fn from_line(bounds: LineDamageBounds) -> Self {
        Self {
            top: bounds.line,
            bottom: bounds.line + 1,
            left: bounds.left,
            right: bounds.right,
        }
    }

    /// Number of rows in this rectangle.
    #[inline]
    #[cfg(test)]
    pub const fn height(self) -> u16 {
        self.bottom.saturating_sub(self.top)
    }

    /// Number of columns in this rectangle.
    #[inline]
    #[cfg(test)]
    pub const fn width(self) -> u16 {
        self.right.saturating_sub(self.left)
    }

    /// Total cells in this rectangle.
    #[inline]
    #[cfg(test)]
    pub const fn cell_count(self) -> u32 {
        self.height() as u32 * self.width() as u32
    }

    /// Check if a line bounds can be merged into this rectangle.
    #[inline]
    pub fn can_extend_with(&self, bounds: LineDamageBounds) -> bool {
        // Line must be immediately below
        if bounds.line != self.bottom {
            return false;
        }
        // Column ranges must overlap or be adjacent
        bounds.left <= self.right && self.left <= bounds.right
    }

    /// Extend this rectangle to include a line bounds.
    #[inline]
    pub fn extend_with(&mut self, bounds: LineDamageBounds) {
        self.bottom = bounds.line + 1;
        self.left = self.left.min(bounds.left);
        self.right = self.right.max(bounds.right);
    }
}
