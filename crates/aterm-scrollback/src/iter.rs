// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Forward iterators over scrollback lines.
//!
//! They stream whole storage segments (a warm block / cold page decodes once
//! and its lines are MOVED out), instead of routing every line through the
//! random-access `get_line` — which paid a binary search, a cache probe, and a
//! full `Line` clone per line on an O(N) sequential walk (ST-6). Newest-first
//! reads use `get_line_rev` directly.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Line, Scrollback, ScrollbackError, ScrollbackStorage};

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

impl Scrollback {
    /// First logical index of the hot tier (`cold + warm` lines): the dense
    /// walk's switch from bulk segment decodes to borrowed hot lines.
    pub(crate) fn hot_start(&self) -> usize {
        self.cold
            .line_count()
            .saturating_add(self.warm.line_count())
    }
}

impl ScrollbackStorage {
    /// DENSE forward walk from logical line `start` (0 = oldest) to the end:
    /// exactly one item per logical line, `None` standing in for a line that
    /// cannot be read.
    ///
    /// The product readers — the search index, its incremental refresh — key
    /// every line by its ABSOLUTE row, so a walk that silently skips a corrupt
    /// segment (as [`iter`](Self::iter) does) would shift every later line
    /// onto its neighbour's coordinate. This one never skips: an undecodable
    /// warm block or cold page yields one `None` per line it spans, and a short
    /// or missing decode yields `None` for each line it failed to supply.
    /// Item `k` is therefore line `start + k`, always, and it equals what the
    /// per-line oracle `get_line(start + k).ok().flatten()` returns — pinned by
    /// the conformance tests in `storage_tests.rs`.
    ///
    /// Cost: one decode per warm block / cold page (lines are MOVED out, never
    /// cloned) and a zero-copy `Cow::Borrowed` per hot line — instead of the
    /// per-line binary search, block-cache probe and `Line` clone `get_line`
    /// pays on every warm/cold line. A corrupt segment is logged once, not
    /// once per line.
    #[must_use]
    pub fn dense_from(&self, start: usize) -> DenseScrollbackIter<'_> {
        let end = self.line_count();
        let hot_start = match self {
            ScrollbackStorage::Memory(sb) => sb.hot_start(),
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => sb.hot_start(),
        };
        DenseScrollbackIter {
            storage: self,
            idx: start.min(end),
            end,
            hot_start: hot_start.min(end),
            buf: VecDeque::new(),
            owed_placeholders: 0,
            placeholders: 0,
        }
    }
}

/// Dense forward walk over a [`ScrollbackStorage`] — see
/// [`ScrollbackStorage::dense_from`]. Yields `Some(line)` for every readable
/// line and `None` as a placeholder for every unreadable one, so the item
/// count always equals `line_count() - start`.
pub struct DenseScrollbackIter<'a> {
    storage: &'a ScrollbackStorage,
    /// Logical index of the NEXT item.
    idx: usize,
    /// `line_count()` when the walk began (the storage is borrowed, so it
    /// cannot change underneath).
    end: usize,
    /// First hot-tier index: below it lines come from bulk segment decodes,
    /// at or above it they are borrowed straight out of the hot tier.
    hot_start: usize,
    /// Decoded lines of the current warm block / cold page, front first.
    buf: VecDeque<Line>,
    /// Placeholder rows still owed for the segment that failed to decode.
    owed_placeholders: usize,
    placeholders: usize,
}

impl DenseScrollbackIter<'_> {
    /// How many placeholders (unreadable lines) the walk has yielded so far.
    #[must_use]
    pub fn placeholders(&self) -> usize {
        self.placeholders
    }

    /// Yield one placeholder for the line at the cursor.
    fn placeholder<'a>(&mut self) -> Option<Option<Cow<'a, Line>>> {
        self.idx = self.idx.saturating_add(1);
        self.placeholders = self.placeholders.saturating_add(1);
        Some(None)
    }
}

impl<'a> Iterator for DenseScrollbackIter<'a> {
    type Item = Option<Cow<'a, Line>>;

    // Skip: the segment-walk driver — like `ScrollbackIter::next`, its bulk
    // reads route into the per-tier decode paths (each individually
    // classified). Conformance-tested against the per-line oracle.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.idx >= self.end {
                return None;
            }
            if self.owed_placeholders > 0 {
                self.owed_placeholders -= 1;
                return self.placeholder();
            }
            if let Some(line) = self.buf.pop_front() {
                self.idx = self.idx.saturating_add(1);
                return Some(Some(Cow::Owned(line)));
            }
            if self.idx >= self.hot_start {
                // Hot tier: uncompressed and in RAM — borrow, never clone.
                return match self.storage.get_line(self.idx) {
                    Ok(Some(line)) => {
                        self.idx = self.idx.saturating_add(1);
                        Some(Some(line))
                    }
                    Ok(None) => self.placeholder(),
                    Err(e) => {
                        aterm_log::warn!(
                            "scrollback dense walk: line {} unreadable: {e}",
                            self.idx
                        );
                        self.placeholder()
                    }
                };
            }
            // Warm/cold: one decode for the whole rest of the segment. The
            // segment never extends past its tier, but clamp to the hot
            // boundary anyway so a malformed decode can never push a line
            // onto another line's coordinate.
            let tier_left = self.hot_start.saturating_sub(self.idx);
            match self.storage.read_segment(self.idx) {
                Ok(mut lines) if !lines.is_empty() => {
                    lines.truncate(tier_left);
                    self.buf = VecDeque::from(lines);
                }
                // A short decode (or a stale count): this line has no data.
                // One placeholder, then the next line is attempted on its own.
                Ok(_) => return self.placeholder(),
                Err((e, skip)) => {
                    aterm_log::warn!(
                        "scrollback dense walk: {skip} unreadable line(s) at {}: {e}",
                        self.idx
                    );
                    self.owed_placeholders = skip.min(tier_left).max(1);
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end.saturating_sub(self.idx);
        (n, Some(n))
    }
}

impl ExactSizeIterator for DenseScrollbackIter<'_> {}

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
    #[cfg(test)]
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
