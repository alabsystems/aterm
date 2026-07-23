// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Forward and reverse iterators over scrollback lines.

use super::{Line, Scrollback};

impl Scrollback {
    /// Iterate over all lines (oldest to newest).
    #[must_use]
    pub fn iter(&self) -> ScrollbackIter<'_> {
        ScrollbackIter {
            scrollback: self,
            idx: 0,
            skipped_lines: 0,
        }
    }

    /// Iterate over recent lines (newest to oldest).
    #[must_use]
    pub fn iter_rev(&self) -> ScrollbackRevIter<'_> {
        ScrollbackRevIter {
            scrollback: self,
            rev_idx: 0,
            skipped_lines: 0,
        }
    }
}

/// Iterator over scrollback lines (oldest to newest).
///
/// When corrupt warm blocks cause decompression errors, affected lines are
/// skipped. Call [`skipped_lines`](Self::skipped_lines) after iteration to
/// detect incomplete results (#5947).
pub struct ScrollbackIter<'a> {
    scrollback: &'a Scrollback,
    idx: usize,
    skipped_lines: usize,
}

impl ScrollbackIter<'_> {
    /// Number of lines skipped due to decompression errors during iteration.
    ///
    /// Non-zero after iteration indicates corrupt warm blocks caused incomplete
    /// results — the iterator yielded fewer items than `line_count()`.
    #[must_use]
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }
}

impl Iterator for ScrollbackIter<'_> {
    type Item = Line;

    // Skip: the tier-walk driver — its line lookups route into the
    // per-tier `get_line`s (each individually classified: guarded-index /
    // decode class). Round-trip and ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        let total = self.scrollback.line_count;
        while self.idx < total {
            match self.scrollback.get_line(self.idx) {
                Ok(Some(cow_line)) => {
                    self.idx += 1;
                    return Some(cow_line.into_owned());
                }
                Ok(None) => return None,
                Err(e) => {
                    aterm_log::warn!("scrollback iter: skipping line {}: {e}", self.idx);
                    self.skipped_lines += 1;
                    self.idx += 1;
                }
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.scrollback.line_count.saturating_sub(self.idx);
        (0, Some(remaining))
    }
}

impl<'a> IntoIterator for &'a Scrollback {
    type Item = Line;
    type IntoIter = ScrollbackIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Reverse iterator over scrollback lines (newest to oldest).
///
/// When corrupt warm blocks cause decompression errors, affected lines are
/// skipped. Call [`skipped_lines`](Self::skipped_lines) after iteration to
/// detect incomplete results (#5947).
pub struct ScrollbackRevIter<'a> {
    scrollback: &'a Scrollback,
    rev_idx: usize,
    skipped_lines: usize,
}

impl ScrollbackRevIter<'_> {
    /// Number of lines skipped due to decompression errors during iteration.
    #[must_use]
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }
}

impl Iterator for ScrollbackRevIter<'_> {
    type Item = Line;

    // Skip: the tier-walk driver — its line lookups route into the
    // per-tier `get_line`s (each individually classified: guarded-index /
    // decode class). Round-trip and ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        let total = self.scrollback.line_count;
        while self.rev_idx < total {
            match self.scrollback.get_line_rev(self.rev_idx) {
                Ok(Some(cow_line)) => {
                    self.rev_idx += 1;
                    return Some(cow_line.into_owned());
                }
                Ok(None) => return None,
                Err(e) => {
                    aterm_log::warn!(
                        "scrollback rev_iter: skipping rev_index {}: {e}",
                        self.rev_idx
                    );
                    self.skipped_lines += 1;
                    self.rev_idx += 1;
                }
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.scrollback.line_count.saturating_sub(self.rev_idx);
        (0, Some(remaining))
    }
}
