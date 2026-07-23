// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid indexing types for terminal coordinates.
//!
//! Provides `Line`, `Column`, `Point`, and related types used to address
//! cells in a terminal grid. Extracted from `aterm-alacritty-bridge` to
//! `aterm-types` so any consumer can use grid coordinates without depending
//! on the bridge crate (#3828).

use std::cmp::{max, min};
use std::fmt;
use std::ops::{Add, Sub};

/// Grid dimensions trait for Point/Line arithmetic.
pub trait Dimensions {
    /// Total lines in the buffer (visible + scrollback).
    fn total_lines(&self) -> usize;
    /// Visible screen lines.
    fn screen_lines(&self) -> usize;
    /// Column count.
    fn columns(&self) -> usize;

    /// Index for the last column.
    #[must_use]
    // Skip: a trait DEFAULT method dispatching to `Self::columns()` — the
    // implementor is caller-chosen (aterm-grid implements `Dimensions` for
    // `Grid`), so the callee is unknowable here. This is the irreducible
    // open-world dispatch class: `Dimensions` is genuinely public and
    // downstream-implementable, so the closed-world rung cannot apply.
    #[cfg_attr(trust_verify, trust::skip)]
    fn last_column(&self) -> Column {
        Column(self.columns().saturating_sub(1))
    }

    /// Topmost line in history.
    #[must_use]
    // Skip: same irreducible open-world dispatch as `last_column` — a trait
    // DEFAULT method calling caller-implemented `Self` methods. `Dimensions`
    // is public and implemented downstream (aterm-grid for `Grid`), so the
    // closed-world rung cannot apply.
    #[cfg_attr(trust_verify, trust::skip)]
    fn topmost_line(&self) -> Line {
        // Clamp to i32::MAX before negation to prevent silent truncation.
        // `try_from(..).unwrap_or(MAX)` is the same clamp as
        // `.min(i32::MAX as usize) as i32`, and `0 - n` with `n >= 0` cannot
        // overflow, so the saturating subtraction is exact.
        let clamped = i32::try_from(self.history_size()).unwrap_or(i32::MAX);
        Line(0i32.saturating_sub(clamped))
    }

    /// Bottommost line in the viewport.
    #[must_use]
    // Skip: same irreducible open-world dispatch as `last_column` — a trait
    // DEFAULT method calling caller-implemented `Self` methods. `Dimensions`
    // is public and implemented downstream (aterm-grid for `Grid`), so the
    // closed-world rung cannot apply.
    #[cfg_attr(trust_verify, trust::skip)]
    fn bottommost_line(&self) -> Line {
        // Clamp to i32::MAX to prevent silent truncation on extreme sizes.
        // `try_from(..).unwrap_or(MAX)` is the same clamp as
        // `.min(i32::MAX as usize) as i32`.
        Line(i32::try_from(self.screen_lines().saturating_sub(1)).unwrap_or(i32::MAX))
    }

    /// Number of lines in scrollback history.
    #[must_use]
    // Skip: same irreducible open-world dispatch as `last_column` — a trait
    // DEFAULT method calling caller-implemented `Self` methods. `Dimensions`
    // is public and implemented downstream (aterm-grid for `Grid`), so the
    // closed-world rung cannot apply.
    #[cfg_attr(trust_verify, trust::skip)]
    fn history_size(&self) -> usize {
        self.total_lines().saturating_sub(self.screen_lines())
    }
}

/// Convenience `Dimensions` for `(lines, columns)` tuples
/// used in tests and lightweight grid simulations.
///
/// All lines are visible (no scrollback): `screen_lines() == total_lines()`.
impl Dimensions for (usize, usize) {
    fn total_lines(&self) -> usize {
        self.0
    }

    fn screen_lines(&self) -> usize {
        self.0
    }

    fn columns(&self) -> usize {
        self.1
    }
}

/// Horizontal direction in the terminal grid.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum Direction {
    /// Left direction.
    Left,
    /// Right direction.
    Right,
}

impl Direction {
    /// Reverse the direction.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Side of a cell for selection anchoring.
pub type Side = Direction;

/// Boundary constraints for cursor/search movement.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum Boundary {
    /// Movement bounded by cursor's range (visible area).
    Cursor,
    /// Movement bounded by entire grid including scrollback.
    #[default]
    Grid,
    /// No boundary constraints.
    None,
}

/// Line index in the terminal grid.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line(pub i32);

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Trust gate: `write!(f, "{}", ..)` needs a runtime-argument
        // `format_args!`, which the native lowering cannot model. The nested
        // `{}` always formats with default options (it never inherits `f`'s
        // flags), so `to_string()` + `write_str` is byte-identical.
        f.write_str(&self.0.to_string())
    }
}

impl From<usize> for Line {
    fn from(value: usize) -> Self {
        Self(value.min(i32::MAX as usize) as i32)
    }
}

impl From<i32> for Line {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl Add<i32> for Line {
    type Output = Self;

    fn add(self, rhs: i32) -> Self::Output {
        Self(self.0.saturating_add(rhs))
    }
}

impl Sub<i32> for Line {
    type Output = Self;

    fn sub(self, rhs: i32) -> Self::Output {
        Self(self.0.saturating_sub(rhs))
    }
}

impl Add<usize> for Line {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        let rhs_clamped = rhs.min(i32::MAX as usize) as i32;
        Self(self.0.saturating_add(rhs_clamped))
    }
}

impl Sub<usize> for Line {
    type Output = Self;

    fn sub(self, rhs: usize) -> Self::Output {
        let rhs_clamped = rhs.min(i32::MAX as usize) as i32;
        Self(self.0.saturating_sub(rhs_clamped))
    }
}

impl Add<Line> for Line {
    type Output = Self;

    fn add(self, rhs: Line) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Sub<Line> for Line {
    type Output = i32;

    fn sub(self, rhs: Line) -> Self::Output {
        self.0.saturating_sub(rhs.0)
    }
}

impl std::ops::SubAssign<i32> for Line {
    fn sub_assign(&mut self, rhs: i32) {
        self.0 = self.0.saturating_sub(rhs);
    }
}

impl std::ops::AddAssign<i32> for Line {
    fn add_assign(&mut self, rhs: i32) {
        self.0 = self.0.saturating_add(rhs);
    }
}

impl Line {
    /// Clamp a line to the grid boundary.
    ///
    /// For `Boundary::Cursor`, clamps to visible screen area [0, bottommost_line].
    /// For `Boundary::Grid`, clamps to total grid including scrollback [topmost_line, bottommost_line].
    /// For `Boundary::None`, wraps around the grid cyclically.
    // Skip: `D: Dimensions` is CALLER-CHOSEN — the clamp reads the grid's
    // dimensions through the public, downstream-implemented `Dimensions` trait
    // (the irreducible open-world dispatch class; aterm-grid supplies the impl).
    // The clamp arithmetic itself is provable and unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn grid_clamp<D: Dimensions>(self, dimensions: &D, boundary: Boundary) -> Self {
        match boundary {
            Boundary::Cursor => max(Line(0), min(dimensions.bottommost_line(), self)),
            Boundary::Grid => {
                let bottommost_line = dimensions.bottommost_line();
                let topmost_line = dimensions.topmost_line();
                max(topmost_line, min(bottommost_line, self))
            }
            Boundary::None => {
                // `try_from(..).unwrap_or(MAX)` is the same clamp as
                // `.min(i32::MAX as usize) as i32`, with a provable range.
                let screen_lines = i32::try_from(dimensions.screen_lines()).unwrap_or(i32::MAX);
                let total_lines = i32::try_from(dimensions.total_lines()).unwrap_or(i32::MAX);

                // `total_lines >= 0` always holds (clamped from a `usize`),
                // so `<= 0` is the same test as `== 0`; it also gives the
                // verifier `total_lines >= 1` for the `%` below.
                if total_lines <= 0 {
                    return self;
                }

                // The saturating ops are exact on every reachable input: in
                // the first branch `self.0 >= screen_lines >= 0` so the
                // subtraction cannot overflow; in the second branch it could
                // only saturate for `Line` values near `i32::MIN`, which no
                // grid can produce (they would panic here pre-change).
                if self.0 >= screen_lines {
                    let topmost_line = dimensions.topmost_line();
                    let extra = self.0.saturating_sub(screen_lines) % total_lines;
                    Line(topmost_line.0.saturating_add(extra))
                } else if self.0 < dimensions.topmost_line().0 {
                    let bottommost_line = dimensions.bottommost_line();
                    let extra = self.0.saturating_sub(screen_lines).saturating_add(1) % total_lines;
                    Line(bottommost_line.0.saturating_add(extra))
                } else {
                    self
                }
            }
        }
    }
}

/// Column index in the terminal grid.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Column(pub usize);

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Trust gate: byte-identical to `write!(f, "{}", self.0)`; see
        // `Display for Line`.
        f.write_str(&self.0.to_string())
    }
}

impl From<usize> for Column {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl Add<usize> for Column {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        Self(self.0.saturating_add(rhs))
    }
}

impl Sub<usize> for Column {
    type Output = Self;

    fn sub(self, rhs: usize) -> Self::Output {
        Self(self.0.saturating_sub(rhs))
    }
}

impl Sub<Column> for Column {
    type Output = usize;

    fn sub(self, rhs: Column) -> Self::Output {
        self.0.saturating_sub(rhs.0)
    }
}

impl std::ops::AddAssign<usize> for Column {
    fn add_assign(&mut self, rhs: usize) {
        self.0 = self.0.saturating_add(rhs);
    }
}

impl std::ops::SubAssign<usize> for Column {
    fn sub_assign(&mut self, rhs: usize) {
        self.0 = self.0.saturating_sub(rhs);
    }
}

/// Grid coordinate expressed as line/column.
// Skip (propagates to the DERIVE-generated impls — the checker consults the
// impl subject for macro-generated items): `L`/`C` are CALLER-CHOSEN type
// params, so the derived Clone/PartialEq/Default dispatch into their impls
// (open-trait user code) — the irreducible open-world class. The defaults
// (Line/Column) are plain integer newtypes; a caller may substitute anything.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Point<L = Line, C = Column> {
    /// Line position.
    pub line: L,
    /// Column position.
    pub column: C,
}

impl<L, C> Point<L, C> {
    /// Create a new point.
    #[must_use]
    pub fn new(line: L, column: C) -> Self {
        Self { line, column }
    }
}

impl<L: Ord, C: Ord> Ord for Point<L, C> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.line.cmp(&other.line) {
            std::cmp::Ordering::Equal => self.column.cmp(&other.column),
            ord => ord,
        }
    }
}

impl<L: Ord, C: Ord> PartialOrd for Point<L, C> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: fmt::Display, C: fmt::Display> fmt::Display for Point<L, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Trust gate: byte-identical to `write!(f, "({}, {})", ..)` — the
        // nested `{}` always uses default options, exactly like `to_string`;
        // see `Display for Line`.
        f.write_str("(")?;
        f.write_str(&self.line.to_string())?;
        f.write_str(", ")?;
        f.write_str(&self.column.to_string())?;
        f.write_str(")")
    }
}

impl Point<Line, Column> {
    /// Subtract a number of columns from a point, wrapping to previous lines as needed.
    ///
    /// The result is clamped according to the boundary constraints.
    pub fn sub<D>(mut self, dimensions: &D, boundary: Boundary, rhs: usize) -> Self
    where
        D: Dimensions,
    {
        let cols = dimensions.columns();
        if cols == 0 {
            return self;
        }

        // Trust discharge: `cols >= 1` (the `cols == 0` early return above), so
        // `wrapping_sub(1)` never wraps. A `Point`'s column is always inside
        // the grid (`self.column.0 < cols`, enforced by `grid_clamp`), so
        // `cols + column` is at most `2 * cols - 1` (no overflow for any real
        // grid width) and `rhs % cols < cols <= cols + column` (no underflow) —
        // the wrapping ops compute exactly what the plain ops did.
        let line_changes = rhs
            .saturating_add(cols.wrapping_sub(1))
            .saturating_sub(self.column.0)
            / cols;
        self.line -= line_changes.min(i32::MAX as usize) as i32;
        self.column = Column(cols.wrapping_add(self.column.0).wrapping_sub(rhs % cols) % cols);
        self.grid_clamp(dimensions, boundary)
    }

    /// Add a number of columns to a point, wrapping to next lines as needed.
    ///
    /// The result is clamped according to the boundary constraints.
    pub fn add<D>(mut self, dimensions: &D, boundary: Boundary, rhs: usize) -> Self
    where
        D: Dimensions,
    {
        let cols = dimensions.columns();
        if cols == 0 {
            return self;
        }

        // Trust discharge: `self.column.0 < cols` (grid invariant, see `sub`)
        // and `rhs % cols < cols`, so the sum is at most `2 * cols - 2` and
        // `wrapping_add` never wraps for any real grid width.
        self.line += (rhs.saturating_add(self.column.0) / cols).min(i32::MAX as usize) as i32;
        self.column = Column(self.column.0.wrapping_add(rhs % cols) % cols);
        self.grid_clamp(dimensions, boundary)
    }

    /// Clamp a point to a grid boundary.
    ///
    /// Ensures the point stays within valid grid coordinates according to the
    /// specified boundary constraints.
    // Skip: `D: Dimensions` is CALLER-CHOSEN — the clamp reads the grid's
    // dimensions through the public, downstream-implemented `Dimensions` trait
    // (the irreducible open-world dispatch class; aterm-grid supplies the impl).
    // The clamp arithmetic itself is provable and unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn grid_clamp<D>(mut self, dimensions: &D, boundary: Boundary) -> Self
    where
        D: Dimensions,
    {
        let last_column = dimensions.last_column();
        self.column = min(self.column, last_column);

        let topmost_line = dimensions.topmost_line();
        let bottommost_line = dimensions.bottommost_line();

        match boundary {
            Boundary::Cursor if self.line.0 < 0 => Point::new(Line(0), Column(0)),
            Boundary::Grid if self.line < topmost_line => Point::new(topmost_line, Column(0)),
            Boundary::Cursor | Boundary::Grid if self.line > bottommost_line => {
                Point::new(bottommost_line, last_column)
            }
            Boundary::None => {
                self.line = self.line.grid_clamp(dimensions, boundary);
                self
            }
            _ => self,
        }
    }
}

/// Alacritty-style scroll requests.
#[non_exhaustive]
#[derive(Debug, Copy, Clone)]
pub enum Scroll {
    /// Scroll by a delta in lines.
    Delta(i32),
    /// Scroll up by one page.
    PageUp,
    /// Scroll down by one page.
    PageDown,
    /// Scroll to top of history.
    Top,
    /// Scroll to bottom (current output).
    Bottom,
}
