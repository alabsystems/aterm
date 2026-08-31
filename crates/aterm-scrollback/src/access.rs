// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared interface for scrollback storage backends.
//!
//! [`ScrollbackAccess`] unifies the 19 methods that both [`Scrollback`] and
//! [`DiskBackedScrollback`] implement with compatible signatures, replacing 38
//! match arms in [`ScrollbackStorage`] with a single `inner()`/`inner_mut()`
//! dispatch pair.
//!
//! Part of #6274.

use std::borrow::Cow;

#[cfg(feature = "disk-tier")]
use super::DiskBackedScrollback;
use super::{Line, Scrollback, ScrollbackError, WatermarkLevel};

/// Private supertrait — the SEALED-trait idiom. `ScrollbackAccess` is an
/// INTERNAL backend abstraction (the memory and disk tiers); it is exported so
/// callers can name it (`&impl ScrollbackAccess`), never so they can implement
/// it. Sealing states that intent in the type system, and it is also what makes
/// the impl set provably CLOSED: the Trust verifier's closed-world rung then
/// discharges every `<S as ScrollbackAccess>::m` dispatch by conjunction over
/// the enumerated impls (both verified panic-free) instead of failing closed on
/// an unresolvable generic callee. Zero annotations, zero behavior change — a
/// downstream `impl` was never valid and is now a compile error.
mod sealed {
    pub trait Sealed {}
}

/// Shared interface for scrollback storage backends.
///
/// All methods that both [`Scrollback`] and [`DiskBackedScrollback`] implement
/// with compatible signatures. The trait uses `io::Result` / `Result<_, ScrollbackError>`
/// for methods where the disk backend can fail — the memory backend wraps
/// infallible calls in `Ok(())`.
///
/// **Sealed**: implementable only inside this crate (see [`sealed`]).
pub trait ScrollbackAccess: sealed::Sealed {
    // --- Read ---

    /// Get the total number of lines across all tiers.
    fn line_count(&self) -> usize;

    /// Get a line by index (0 = oldest).
    fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError>;

    /// Get a line by reverse index (0 = newest).
    fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError>;

    // --- Config (read) ---

    /// Get the hot tier limit.
    fn hot_limit(&self) -> usize;

    /// Get the warm tier limit.
    fn warm_limit(&self) -> usize;

    /// Get the line limit (maximum total lines allowed).
    fn line_limit(&self) -> Option<usize>;

    /// Get the memory budget.
    fn memory_budget(&self) -> usize;

    // --- Metrics ---

    /// Get the hot+warm memory usage (bytes).
    fn memory_used(&self) -> usize;

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

    /// History lines whose inline image was dropped at the image retention
    /// horizon (both backends compress at the same boundary, so both count it).
    fn image_rows_dropped_by_compression(&self) -> u64;

    // --- Write ---

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

    // --- Checkpoint ---

    /// Create an in-memory snapshot of hot + warm tiers (skip cold).
    fn checkpoint_snapshot_fast(&self) -> Scrollback;
}

/// Shared implementation for `checkpoint_snapshot_fast()`.
///
/// Both [`Scrollback`] and [`DiskBackedScrollback`] iterate warm + hot lines,
/// skipping the cold tier for bounded checkpoint latency. (#5946, #6714)
// Skip: the snapshot walk drives the sealed-trait accessors plus Vec/Line
// clones (idiomatic-alloc class). Round-trip tested by the checkpoint gate.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn checkpoint_snapshot_fast_from(source: &impl ScrollbackAccess) -> Scrollback {
    let mut snapshot = Scrollback::new(
        source.hot_limit(),
        source.warm_limit(),
        source.memory_budget(),
    );
    snapshot.set_line_limit(source.line_limit());

    let cold_count = source.cold_line_count();
    let total = source.line_count();
    let mut saved: usize = 0;
    let mut skipped: usize = 0;

    for idx in cold_count..total {
        match source.get_line(idx) {
            Ok(Some(cow_line)) => {
                snapshot.push_line(cow_line.into_owned());
                saved += 1;
            }
            Ok(None) => break,
            Err(e) => {
                aterm_log::warn!("checkpoint_snapshot_fast: skipping line {idx}: {e}");
                skipped += 1;
            }
        }
    }

    if skipped > 0 {
        aterm_log::warn!(
            "checkpoint_snapshot_fast: {saved} lines saved, {skipped} skipped, \
             {cold_count} cold lines excluded"
        );
    }

    snapshot
}

// ---------------------------------------------------------------------------
// impl ScrollbackAccess for Scrollback
// ---------------------------------------------------------------------------

impl sealed::Sealed for Scrollback {}

impl ScrollbackAccess for Scrollback {
    fn line_count(&self) -> usize {
        self.line_count()
    }

    fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line(idx)
    }

    fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line_rev(rev_idx)
    }

    fn hot_limit(&self) -> usize {
        self.hot_limit()
    }

    fn warm_limit(&self) -> usize {
        self.warm_limit()
    }

    fn line_limit(&self) -> Option<usize> {
        self.line_limit()
    }

    fn memory_budget(&self) -> usize {
        self.memory_budget()
    }

    fn memory_used(&self) -> usize {
        self.memory_used()
    }

    fn budgeted_memory_used(&self) -> usize {
        self.budgeted_memory_used()
    }

    fn total_memory_used(&self) -> usize {
        self.total_memory_used()
    }

    fn cold_line_count(&self) -> usize {
        self.cold_line_count()
    }

    fn cold_memory_used(&self) -> usize {
        self.cold_memory_used()
    }

    fn watermark_level(&self) -> WatermarkLevel {
        self.watermark_level()
    }

    fn image_rows_dropped_by_compression(&self) -> u64 {
        self.image_rows_dropped_by_compression()
    }

    fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        // Inherent push_line returns (), wrap in Ok for trait compatibility.
        Scrollback::push_line(self, line);
        Ok(())
    }

    fn clear(&mut self) -> std::io::Result<()> {
        // Inherent clear returns (), wrap in Ok for trait compatibility.
        Scrollback::clear(self);
        Ok(())
    }

    fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        self.remove_newest(n)
    }

    fn set_line_limit(&mut self, limit: Option<usize>) {
        self.set_line_limit(limit);
    }

    fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.set_memory_budget(budget)
    }

    fn checkpoint_snapshot_fast(&self) -> Scrollback {
        self.checkpoint_snapshot_fast()
    }
}

// ---------------------------------------------------------------------------
// impl ScrollbackAccess for DiskBackedScrollback
// ---------------------------------------------------------------------------

#[cfg(feature = "disk-tier")]
impl sealed::Sealed for DiskBackedScrollback {}

#[cfg(feature = "disk-tier")]
impl ScrollbackAccess for DiskBackedScrollback {
    fn line_count(&self) -> usize {
        self.line_count()
    }

    fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line(idx)
    }

    fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        self.get_line_rev(rev_idx)
    }

    fn hot_limit(&self) -> usize {
        self.hot_limit()
    }

    fn warm_limit(&self) -> usize {
        self.warm_limit()
    }

    fn line_limit(&self) -> Option<usize> {
        self.line_limit()
    }

    fn memory_budget(&self) -> usize {
        self.memory_budget()
    }

    fn memory_used(&self) -> usize {
        self.memory_used()
    }

    fn budgeted_memory_used(&self) -> usize {
        self.budgeted_memory_used()
    }

    fn total_memory_used(&self) -> usize {
        self.total_memory_used()
    }

    fn cold_line_count(&self) -> usize {
        self.cold_line_count()
    }

    fn cold_memory_used(&self) -> usize {
        self.cold_memory_used()
    }

    fn watermark_level(&self) -> WatermarkLevel {
        self.watermark_level()
    }

    fn image_rows_dropped_by_compression(&self) -> u64 {
        self.image_rows_dropped_by_compression()
    }

    fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        self.push_line(line)
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.clear()
    }

    fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        self.remove_newest(n)
    }

    fn set_line_limit(&mut self, limit: Option<usize>) {
        self.set_line_limit(limit);
    }

    fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.set_memory_budget(budget)
    }

    fn checkpoint_snapshot_fast(&self) -> Scrollback {
        self.checkpoint_snapshot_fast()
    }
}
