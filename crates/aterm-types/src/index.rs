// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid indexing types for terminal coordinates.
//!
//! Provides `Line`, `Column`, `Point`, and related types used to address
//! cells in a terminal grid. Extracted from `aterm-alacritty-bridge` to
//! `aterm-types` so any consumer can use grid coordinates without depending
//! on the bridge crate (#3828).

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
