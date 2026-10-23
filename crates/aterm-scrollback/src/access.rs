// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`ScrollbackAccess`]: shared trait dispatched by
//! [`ScrollbackStorage`](super::ScrollbackStorage) across concrete backends.

use std::borrow::Cow;

use super::{Line, Scrollback, ScrollbackError, WatermarkLevel};

/// Sealed supertrait so the Trust verifier can enumerate all [`ScrollbackAccess`] impls.
mod sealed {
    pub trait Sealed {}
}

/// Shared interface for scrollback storage backends (infallible memory operations wrap `Ok(())`).
pub trait ScrollbackAccess: sealed::Sealed {
    /// Get the total number of lines across all tiers.
    fn line_count(&self) -> usize;

    /// Get a line by index (0 = oldest).
    fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError>;

    /// Get a line by reverse index (0 = newest).
    fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError>;

    /// Get the hot tier limit.
    fn hot_limit(&self) -> usize;

    /// Get the warm tier limit.
    fn warm_limit(&self) -> usize;

    /// Get the line limit (maximum total lines allowed).
    fn line_limit(&self) -> Option<usize>;

    /// Get the memory budget.
    fn memory_budget(&self) -> usize;

    /// Get reclaimable storage bytes used for budget enforcement.
    fn budgeted_memory_used(&self) -> usize;

    /// Get total memory usage across all tiers (bytes).
    fn total_memory_used(&self) -> usize;

    /// Get the number of lines in cold tier.
    fn cold_line_count(&self) -> usize;

    /// Get cold tier memory usage (bytes, compressed).
    fn cold_memory_used(&self) -> usize;

    /// Get the current memory pressure watermark level.
    fn watermark_level(&self) -> WatermarkLevel;

    /// History lines whose inline image was dropped at the image retention horizon.
    fn image_rows_dropped_by_compression(&self) -> u64;

    /// Push a new line to the scrollback.
    fn push_line(&mut self, line: Line) -> std::io::Result<()>;

    /// Clear all lines from scrollback.
    fn clear(&mut self) -> std::io::Result<()>;

    /// Remove the `n` most recent lines from scrollback.
    fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError>;

    /// Set the line limit.
    fn set_line_limit(&mut self, limit: Option<usize>);

    /// Set the memory budget (bytes).
    fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError>;
}

impl sealed::Sealed for Scrollback {}

impl ScrollbackAccess for Scrollback {
    #[inline]
    fn line_count(&self) -> usize {
        self.line_count()
    }

    #[inline]
    fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line(idx)
    }

    #[inline]
    fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line_rev(rev_idx)
    }

    #[inline]
    fn hot_limit(&self) -> usize {
        self.hot_limit()
    }

    #[inline]
    fn warm_limit(&self) -> usize {
        self.warm_limit()
    }

    #[inline]
    fn line_limit(&self) -> Option<usize> {
        self.line_limit()
    }

    #[inline]
    fn memory_budget(&self) -> usize {
        self.memory_budget()
    }

    #[inline]
    fn budgeted_memory_used(&self) -> usize {
        self.budgeted_memory_used()
    }

    #[inline]
    fn total_memory_used(&self) -> usize {
        self.total_memory_used()
    }

    #[inline]
    fn cold_line_count(&self) -> usize {
        self.cold_line_count()
    }

    #[inline]
    fn cold_memory_used(&self) -> usize {
        self.cold_memory_used()
    }

    #[inline]
    fn watermark_level(&self) -> WatermarkLevel {
        self.watermark_level()
    }

    #[inline]
    fn image_rows_dropped_by_compression(&self) -> u64 {
        self.image_rows_dropped_by_compression()
    }

    #[inline]
    fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        Scrollback::push_line(self, line);
        Ok(())
    }

    #[inline]
    fn clear(&mut self) -> std::io::Result<()> {
        Scrollback::clear(self);
        Ok(())
    }

    #[inline]
    fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        self.remove_newest(n)
    }

    #[inline]
    fn set_line_limit(&mut self, limit: Option<usize>) {
        self.set_line_limit(limit);
    }

    #[inline]
    fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.set_memory_budget(budget)
    }
}
