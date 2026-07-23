// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Hot tier: uncompressed lines in VecDeque.
//!
//! Instant access, no decompression overhead.

use super::line::Line;
use std::collections::VecDeque;

/// Hot tier: uncompressed lines in RAM.
///
/// Uses VecDeque for efficient front/back operations.
#[derive(Debug)]
pub(crate) struct HotTier {
    /// Lines stored uncompressed.
    lines: VecDeque<Line>,
    /// Running total for `memory_used()` (diagnostic: includes struct overhead).
    bytes_used: usize,
    /// Reclaimable line storage only (budget enforcement). Excludes struct overhead.
    budgeted_bytes: usize,
}

impl HotTier {
    /// Create a new hot tier.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            bytes_used: std::mem::size_of::<Self>(),
            budgeted_bytes: 0,
        }
    }

    /// Get the number of lines.
    #[must_use]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    /// Push a line to the back.
    ///
    /// Spelled `saturating_add`: the counters track bytes of memory this
    /// process actually holds, so the true sum always fits in `usize` and the
    /// saturation can never fire on a real path — it just discharges the
    /// strict L0 gate's unconstrained-input overflow counterexamples.
    #[inline]
    pub(crate) fn push(&mut self, line: Line) {
        let mem = line.memory_used();
        self.bytes_used = self.bytes_used.saturating_add(mem);
        self.budgeted_bytes = self.budgeted_bytes.saturating_add(mem);
        self.lines.push_back(line);
    }

    /// Get a line by index (0 = oldest).
    ///
    /// Returns a reference — no clone. Callers that need ownership should
    /// clone explicitly or use `Cow::into_owned()` at the tier-dispatch level.
    #[must_use]
    // Skip: the ring index read — VecDeque::get navigates the ring's head/len
    // arithmetic the verifier cannot chain. Total by the `Option` return.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get(&self, idx: usize) -> Option<&Line> {
        self.lines.get(idx)
    }

    /// Take n lines from the front.
    pub(crate) fn take_front(&mut self, n: usize) -> Vec<Line> {
        let n = n.min(self.lines.len());
        // Capacity is only a hint (never affects the returned lines). Real
        // callers pass block-size-scale counts (`block_size` is clamped to
        // MAX_DECODE_PAGE_LINES at construction), so the bounded branch is the
        // one that always runs; the bound just makes the strict L0 gate's
        // allocation-budget obligation provable.
        let mut result = if n <= crate::line::MAX_DECODE_PAGE_LINES {
            Vec::with_capacity(n)
        } else {
            Vec::new()
        };
        for _ in 0..n {
            if let Some(line) = self.lines.pop_front() {
                let mem = line.memory_used();
                self.bytes_used = self.bytes_used.saturating_sub(mem);
                self.budgeted_bytes = self.budgeted_bytes.saturating_sub(mem);
                result.push(line);
            }
        }
        result
    }

    /// Truncate to keep only the last n lines.
    // Skip: the ring `drain(..n)` under its `n <= len` guard — the
    // BLANKET-unmodeled drain class (guards don't chain). Unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_front(&mut self, n: usize) {
        while self.lines.len() > n {
            if let Some(line) = self.lines.pop_front() {
                let mem = line.memory_used();
                self.bytes_used = self.bytes_used.saturating_sub(mem);
                self.budgeted_bytes = self.budgeted_bytes.saturating_sub(mem);
            }
        }
    }

    /// Remove the `n` most recent lines (from the back).
    ///
    /// Used by Kitty CSI +T unscroll: recovered lines must be removed
    /// from scrollback after being placed back into the visible grid.
    // Skip: the ring truncate's blanket-drain class (twin of truncate_front).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_back(&mut self, n: usize) {
        for _ in 0..n.min(self.lines.len()) {
            if let Some(line) = self.lines.pop_back() {
                let mem = line.memory_used();
                self.bytes_used = self.bytes_used.saturating_sub(mem);
                self.budgeted_bytes = self.budgeted_bytes.saturating_sub(mem);
            }
        }
    }

    /// Clear all lines.
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.bytes_used = std::mem::size_of::<Self>();
        self.budgeted_bytes = 0;
    }

    /// Calculate memory used (diagnostic: includes struct overhead).
    #[must_use]
    pub(crate) fn memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Reclaimable line storage bytes (budget enforcement only).
    #[must_use]
    #[inline]
    pub(crate) fn budgeted_bytes(&self) -> usize {
        self.budgeted_bytes
    }

    /// Saturating fold for the same strict-gate reason as
    /// [`push`](Self::push) (identical on every real path).
    ///
    /// cfg: callers are the test suite and `DiskBackedScrollback`'s
    /// debug-build invariant recomputation (`disk-tier`); `Scrollback`'s own
    /// recomputation is test-only, so this exact cfg avoids dead code in
    /// non-test default-feature debug builds.
    #[cfg(any(test, all(debug_assertions, feature = "disk-tier")))]
    #[must_use]
    pub(crate) fn recompute_memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let mut lines_mem = 0usize;
        for line in &self.lines {
            lines_mem = lines_mem.saturating_add(line.memory_used());
        }
        base.saturating_add(lines_mem)
    }

    /// See [`recompute_memory_used`](Self::recompute_memory_used) for the
    /// cfg and saturating-fold rationale.
    #[cfg(any(test, all(debug_assertions, feature = "disk-tier")))]
    #[must_use]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        let mut lines_mem = 0usize;
        for line in &self.lines {
            lines_mem = lines_mem.saturating_add(line.memory_used());
        }
        lines_mem
    }
}

impl Default for HotTier {
    fn default() -> Self {
        Self::new()
    }
}
