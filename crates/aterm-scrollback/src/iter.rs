// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Forward and reverse iterators over scrollback lines.
//!
//! The FORWARD iterators stream whole storage segments (a warm block / cold
//! page decodes once and its lines are MOVED out), instead of routing every
//! line through the random-access `get_line` — which paid a binary search, a
//! cache probe, and a full `Line` clone per line on an O(N) sequential walk
//! (ST-6). The reverse iterator keeps the per-line path: reverse walks are
//! short (`iter_rev().take(k)`) and a reversed block stream would buy nothing
//! there.

use std::collections::VecDeque;

use super::{Line, Scrollback, ScrollbackError};

/// One bulk read for the streaming walk: the owned lines from a requested
/// index through the end of its storage segment, or the error plus how many
/// logical lines to skip past the undecodable segment. The skip count equals
/// what the old per-line walk skipped one `get_line` error at a time, so
/// `skipped_lines` totals are unchanged.
pub(crate) type SegmentResult = Result<Vec<Line>, (ScrollbackError, usize)>;

impl Scrollback {
    /// Iterate over all lines (oldest to newest).
    #[must_use]
    pub fn iter(&self) -> ScrollbackIter<'_> {
        ScrollbackIter {
            scrollback: self,
            idx: 0,
            skipped_lines: 0,
            buf: VecDeque::new(),
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

    /// Bulk read for the streaming iterators: owned lines from `idx` through
    /// the end of its tier segment (cold page / warm block / one hot line).
    ///
    /// Tier dispatch mirrors `get_line`; the hot tier yields single cloned
    /// lines (it is uncompressed and bounded by `hot_limit`, and cloning per
    /// line is exactly what the old walk did there).
    // Skip: the tier-dispatch driver — routes into the per-tier bulk reads
    // (each individually classified: guarded-index / decode class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn read_segment(&self, idx: usize) -> SegmentResult {
        let cold_count = self.cold.line_count();
        let warm_count = self.warm.line_count();

        if idx < cold_count {
            self.cold.take_lines_from(idx).map_err(|e| {
                // Skip the rest of the undecodable page, clamped into the
                // cold range; `max(1)` guarantees forward progress even
                // against a degenerate segment length.
                let skip = self
                    .cold
                    .segment_len_at(idx)
                    .min(cold_count.saturating_sub(idx))
                    .max(1);
                (e, skip)
            })
        } else if idx < cold_count.saturating_add(warm_count) {
            let warm_idx = idx.saturating_sub(cold_count);
            self.warm.take_lines_from(warm_idx).map_err(|e| {
                let skip = self
                    .warm
                    .segment_len_at(warm_idx)
                    .min(warm_count.saturating_sub(warm_idx))
                    .max(1);
                (e, skip)
            })
        } else {
            let hot_idx = idx.saturating_sub(cold_count).saturating_sub(warm_count);
            match self.hot.get(hot_idx) {
                Some(line) => Ok(vec![line.clone()]),
                // In-range index with no hot line: stale aggregate count —
                // surface as end-of-data exactly like the old `Ok(None)`.
                None => Ok(Vec::new()),
            }
        }
    }
}

/// Iterator over scrollback lines (oldest to newest).
///
/// Streams whole decoded segments: each warm block / cold page is decoded
/// ONCE and its lines are moved out — no per-line binary search or clone.
///
/// When corrupt warm blocks cause decompression errors, affected lines are
/// skipped (one warning per corrupt segment, not per line; the per-line
/// SKIP COUNT is unchanged). Call [`skipped_lines`](Self::skipped_lines)
/// after iteration to detect incomplete results (#5947).
pub struct ScrollbackIter<'a> {
    scrollback: &'a Scrollback,
    idx: usize,
    skipped_lines: usize,
    /// Decoded lines of the current segment, drained front-to-back. Owned:
    /// yielding is a move, never a clone.
    buf: VecDeque<Line>,
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

    // Skip: the segment-walk driver — its bulk reads route into the per-tier
    // decode paths (each individually classified: guarded-index / decode
    // class). Round-trip, parity-oracle, and ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.buf.pop_front() {
                // Saturating cursor idiom (see ScrollbackStorageIter).
                self.idx = self.idx.saturating_add(1);
                return Some(line);
            }
            let total = self.scrollback.line_count;
            if self.idx >= total {
                return None;
            }
            match self.scrollback.read_segment(self.idx) {
                Ok(lines) => {
                    if lines.is_empty() {
                        // In-range index with no data: stale aggregate count
                        // or short decode — end-of-data, like the old
                        // `Ok(None)` arm.
                        return None;
                    }
                    self.buf = VecDeque::from(lines);
                }
                Err((e, skip)) => {
                    aterm_log::warn!(
                        "scrollback iter: skipping {skip} line(s) at {}: {e}",
                        self.idx
                    );
                    // Clamp so a corrupt segment at the tail cannot push the
                    // cursor past `total` and over-count skips.
                    let skip = skip.min(total.saturating_sub(self.idx)).max(1);
                    self.skipped_lines = self.skipped_lines.saturating_add(skip);
                    self.idx = self.idx.saturating_add(skip);
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.scrollback.line_count.saturating_sub(self.idx);
        // `idx` counts CONSUMED lines including the current segment's yielded
        // prefix, so `remaining` already accounts for the buffered tail.
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
