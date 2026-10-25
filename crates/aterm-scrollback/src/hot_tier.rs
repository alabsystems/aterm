// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hot tier: uncompressed lines in a `VecDeque`, no decompression on access.

use super::line::{ImageSpan, Line};
use std::collections::VecDeque;

/// Hot tier: uncompressed lines in RAM.
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
    #[inline]
    pub(crate) const fn new() -> Self {
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
    #[inline]
    pub(crate) fn push(&mut self, line: Line) {
        let mem = line.memory_used();
        self.bytes_used = self.bytes_used.saturating_add(mem);
        self.budgeted_bytes = self.budgeted_bytes.saturating_add(mem);
        self.lines.push_back(line);
    }

    /// Get a line by index (0 = oldest), by reference.
    #[must_use]
    // Skip: the ring index read — VecDeque::get navigates the ring's head/len
    // arithmetic the verifier cannot chain. Total by the `Option` return.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub(crate) fn get(&self, idx: usize) -> Option<&Line> {
        self.lines.get(idx)
    }

    /// Whether an uncompressed line holds an image accepted by `selected`.
    pub(crate) fn any_image(&self, selected: &dyn Fn(&ImageSpan) -> bool) -> bool {
        self.lines.iter().any(|line| {
            line.images()
                .is_some_and(|images| images.iter().any(selected))
        })
    }

    /// Retain selected image spans and reconcile both running byte counts.
    pub(crate) fn retain_images(&mut self, keep: &mut dyn FnMut(&ImageSpan) -> bool) -> bool {
        let mut changed = false;
        for line in &mut self.lines {
            let before = line.memory_used();
            if line.retain_images(keep) {
                let after = line.memory_used();
                self.bytes_used = self.bytes_used.saturating_sub(before).saturating_add(after);
                self.budgeted_bytes = self
                    .budgeted_bytes
                    .saturating_sub(before)
                    .saturating_add(after);
                changed = true;
            }
        }
        changed
    }

    /// Take n lines from the front.
    pub(crate) fn take_front(&mut self, n: usize) -> Vec<Line> {
        let n = n.min(self.lines.len());
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
    // Skip: the ring `pop_front` loop under its `len > n` guard, an unmodeled
    // VecDeque op (guards don't chain). Unit-tested.
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

    /// Remove the `n` most recent lines from the back.
    // Skip: the same ring-pop class as truncate_front.
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
    #[inline]
    pub(crate) const fn memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Reclaimable line storage bytes (budget enforcement only).
    #[must_use]
    #[inline]
    pub(crate) const fn budgeted_bytes(&self) -> usize {
        self.budgeted_bytes
    }

    /// Saturating for the same Trust-lane reason as [`push`](Self::push).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn recompute_memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let mut lines_mem = 0usize;
        for line in &self.lines {
            lines_mem = lines_mem.saturating_add(line.memory_used());
        }
        base.saturating_add(lines_mem)
    }

    /// Recompute `budgeted_bytes` from the lines; cfg and saturation as in
    /// [`recompute_memory_used`](Self::recompute_memory_used).
    #[cfg(test)]
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
