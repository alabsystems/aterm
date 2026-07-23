// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Vi mode types: motions, coordinates, search state, and marks.
//!
//! These types are independent of the grid implementation and can be
//! used by both aterm-core internals and external consumers (the
//! Alacritty bridge, FFI callers, etc.).

#[cfg(not(kani))]
use std::collections::HashMap;

use aterm_types::BufferCommand;

/// Vi marks storage — uses VerifyMap (BTreeMap-based) under Kani to avoid
/// CCRandomGenerateBytes FFI issue from HashMap's RandomState (#5889).
#[cfg(kani)]
type ViMarksMap = aterm_types::verification::stubs::VerifyMap<char, ViPoint>;
#[cfg(not(kani))]
type ViMarksMap = HashMap<char, ViPoint>;

// ---------------------------------------------------------------------------
// Coordinates
// ---------------------------------------------------------------------------

/// A scrollback-aware point for vi mode navigation.
///
/// Row 0 is the top of the visible screen. Negative values index into
/// scrollback. The column is 0-indexed.
///
/// This decouples vi navigation from the grid's internal `u16` row/col
/// coordinate system, allowing motions to operate across the visible
/// screen and scrollback uniformly.
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Default)]
pub struct ViPoint {
    /// Logical line: 0 = visible top, negative = scrollback.
    pub line: i32,
    /// Column (0-indexed).
    pub col: u16,
}

impl ViPoint {
    /// Create a new `ViPoint`.
    #[must_use]
    pub fn new(line: i32, col: u16) -> Self {
        Self { line, col }
    }
}

// ---------------------------------------------------------------------------
// Direction
// ---------------------------------------------------------------------------

/// Movement direction used by motions and searches.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ViDirection {
    /// Towards the start (left / up).
    Left,
    /// Towards the end (right / down).
    Right,
}

// ---------------------------------------------------------------------------
// Motion enum
// ---------------------------------------------------------------------------

/// Vi mode motion commands.
///
/// Each variant represents a single motion that can be dispatched to
/// the vi mode cursor. Complex motions (word, bracket, paragraph,
/// search) require grid content access; the cursor applies basic
/// motions (up/down/left/right/H/M/L/0/$) directly.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ViMotion {
    /// Move cursor up (k).
    Up,
    /// Move cursor down (j).
    Down,
    /// Move cursor left (h).
    Left,
    /// Move cursor right (l).
    Right,
    /// Move to first column (0) or beginning of wrapped line.
    First,
    /// Move to last column ($) or end of wrapped line.
    Last,
    /// Move to first non-whitespace cell in line (^).
    FirstOccupied,
    /// Move to top of visible screen (H).
    High,
    /// Move to center of visible screen (M).
    Middle,
    /// Move to bottom of visible screen (L).
    Low,
    /// Start of previous semantic word (b).
    SemanticLeft,
    /// Start of next semantic word (w).
    SemanticRight,
    /// End of previous semantic word (ge).
    SemanticLeftEnd,
    /// End of current/next semantic word (e).
    SemanticRightEnd,
    /// Start of previous whitespace-separated word (B).
    WordLeft,
    /// Start of next whitespace-separated word (W).
    WordRight,
    /// End of previous whitespace-separated word (gE).
    WordLeftEnd,
    /// End of current/next whitespace-separated word (E).
    WordRightEnd,
    /// Jump to matching bracket (%).
    Bracket,
    /// Move above current paragraph (empty line, `{`).
    ParagraphUp,
    /// Move below current paragraph (empty line, `}`).
    ParagraphDown,
    /// Jump to the next search match (n).
    SearchNext,
    /// Jump to the previous search match (N).
    SearchPrevious,
    /// Jump to exact mark position (`` ` ``).
    GotoMark(char),
    /// Jump to first non-blank of marked line (`'`).
    GotoMarkLine(char),
}

impl ViMotion {
    /// Returns the primary direction of this motion.
    #[must_use]
    pub fn direction(self) -> ViDirection {
        match self {
            Self::Up
            | Self::Left
            | Self::First
            | Self::High
            | Self::SemanticLeft
            | Self::SemanticLeftEnd
            | Self::WordLeft
            | Self::WordLeftEnd
            | Self::ParagraphUp
            | Self::SearchPrevious => ViDirection::Left,

            Self::Down
            | Self::Right
            | Self::Last
            | Self::FirstOccupied
            | Self::Middle
            | Self::Low
            | Self::SemanticRight
            | Self::SemanticRightEnd
            | Self::WordRight
            | Self::WordRightEnd
            | Self::Bracket
            | Self::ParagraphDown
            | Self::SearchNext => ViDirection::Right,

            // Mark motions don't have a fixed direction; default Right.
            Self::GotoMark(_) | Self::GotoMarkLine(_) => ViDirection::Right,
        }
    }
}

impl From<ViMotion> for BufferCommand {
    fn from(motion: ViMotion) -> Self {
        match motion {
            ViMotion::Up => Self::PreviousLine,
            ViMotion::Down => Self::NextLine,
            ViMotion::Left => Self::BackwardChar,
            ViMotion::Right => Self::ForwardChar,
            ViMotion::First => Self::BeginningOfLine,
            ViMotion::Last => Self::EndOfLine,
            ViMotion::FirstOccupied => Self::FirstNonBlank,
            ViMotion::High => Self::ScreenTop,
            ViMotion::Middle => Self::ScreenMiddle,
            ViMotion::Low => Self::ScreenBottom,
            ViMotion::SemanticLeft => Self::BackwardWord,
            ViMotion::SemanticRight => Self::ForwardWord,
            ViMotion::SemanticLeftEnd => Self::BackwardWordEnd,
            ViMotion::SemanticRightEnd => Self::ForwardWordEnd,
            ViMotion::WordLeft => Self::BackwardWordBig,
            ViMotion::WordRight => Self::ForwardWordBig,
            ViMotion::WordLeftEnd => Self::BackwardWordEndBig,
            ViMotion::WordRightEnd => Self::ForwardWordEndBig,
            ViMotion::Bracket => Self::MatchBracket,
            ViMotion::ParagraphUp => Self::ParagraphUp,
            ViMotion::ParagraphDown => Self::ParagraphDown,
            ViMotion::SearchNext => Self::SearchNext,
            ViMotion::SearchPrevious => Self::SearchPrevious,
            ViMotion::GotoMark(c) => Self::GotoMark(c),
            ViMotion::GotoMarkLine(c) => Self::GotoMarkLine(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Visual selection type
// ---------------------------------------------------------------------------

/// The type of vi visual selection mode.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ViVisualType {
    /// Character-wise selection (v).
    Char,
    /// Line-wise selection (V).
    Line,
    /// Block/column selection (Ctrl+V).
    Block,
}

// ---------------------------------------------------------------------------
// Boundary
// ---------------------------------------------------------------------------

/// Boundary behavior for left/right wrapping.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ViBoundary {
    /// Stop at line edges (normal mode).
    Grid,
    /// Wrap across lines (for selection expansion, etc.).
    None,
}

// ---------------------------------------------------------------------------
// Inline search (f/F/t/T)
// ---------------------------------------------------------------------------

/// The type of inline character search.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InlineSearchKind {
    /// Find character to the right (f).
    FindRight,
    /// Find character to the left (F).
    FindLeft,
    /// Move before character to the right (t).
    TillRight,
    /// Move after character to the left (T).
    TillLeft,
}

impl InlineSearchKind {
    /// Returns the direction of this search kind.
    #[must_use]
    pub fn direction(self) -> ViDirection {
        match self {
            Self::FindRight | Self::TillRight => ViDirection::Right,
            Self::FindLeft | Self::TillLeft => ViDirection::Left,
        }
    }

    /// Returns this search kind with the direction reversed.
    #[must_use]
    pub fn reversed(self) -> Self {
        match self {
            Self::FindRight => Self::FindLeft,
            Self::FindLeft => Self::FindRight,
            Self::TillRight => Self::TillLeft,
            Self::TillLeft => Self::TillRight,
        }
    }

    /// Returns whether this is a "till" search (t/T) vs "find" search (f/F).
    #[must_use]
    pub fn is_till(self) -> bool {
        matches!(self, Self::TillRight | Self::TillLeft)
    }
}

/// Inline character search state for f/F/t/T motions.
///
/// Stores the last search so it can be repeated with `;` (same) or `,` (reverse).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct InlineSearchState {
    /// The character that was searched for.
    pub char: char,
    /// The type of search that was performed.
    pub kind: InlineSearchKind,
}

// ---------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------

/// Vi mode marks storage.
///
/// Supports lowercase marks (a–z) and the special `` ` ``/`'` marks.
#[derive(Debug, Clone, Default)]
pub struct ViMarks {
    marks: ViMarksMap,
}

impl ViMarks {
    /// Create an empty mark set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a mark. Returns `true` if the character is valid (a–z, `` ` ``, `'`).
    pub fn set(&mut self, mark: char, point: ViPoint) -> bool {
        if mark.is_ascii_lowercase() || mark == '\'' || mark == '`' {
            self.marks.insert(mark, point);
            true
        } else {
            false
        }
    }

    /// Get the position of a mark.
    #[must_use]
    pub fn get(&self, mark: char) -> Option<ViPoint> {
        self.marks.get(&mark).copied()
    }

    /// Remove a mark, returning the old position.
    pub fn remove(&mut self, mark: char) -> Option<ViPoint> {
        self.marks.remove(&mark)
    }

    /// Clear all marks.
    pub fn clear(&mut self) {
        self.marks.clear();
    }

    /// Check if a mark is set.
    #[must_use]
    pub fn contains(&self, mark: char) -> bool {
        self.marks.contains_key(&mark)
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
