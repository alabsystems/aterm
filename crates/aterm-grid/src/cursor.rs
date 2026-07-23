// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cursor types for terminal grid positioning.

/// Cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// Row (0-indexed, from top of visible area).
    pub row: u16,
    /// Column (0-indexed).
    pub col: u16,
}

impl Cursor {
    /// Create a new cursor at the given position.
    #[must_use]
    #[inline]
    pub const fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

/// Saved cursor state (for DECSC/DECRC).
#[derive(Debug, Clone, Copy, Default)]
pub struct SavedCursor {
    /// Cursor position.
    pub cursor: Cursor,
    /// Whether a saved cursor exists.
    pub valid: bool,
    /// Pending wrap state at time of save (xterm saves wrapnext with DECSC).
    pub pending_wrap: bool,
}
