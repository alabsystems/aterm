// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Forward iterators over scrollback lines.
//!
//! They stream whole storage segments (a warm block or cold page decodes once and
//! its lines are moved out) instead of calling `get_line` per line. Newest-first
//! reads use `get_line_rev`.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Line, Scrollback, ScrollbackError, ScrollbackStorage};

/// One bulk read for the streaming walk: the owned lines from a requested index
/// through the end of its storage segment, or the error plus how many logical
/// lines to skip past the undecodable segment.
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
    /// the end of its tier segment (cold page / warm block / one cloned hot line).
    // Skip: a thin forwarder to `read_segment_tiers` (same class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn read_segment(&self, idx: usize) -> SegmentResult {
        read_segment_tiers(&self.cold, &self.warm, &self.hot, idx)
    }

    /// First logical index of the hot tier (`cold + warm` lines): the dense
    /// walk's switch from bulk segment decodes to borrowed hot lines.
    pub(crate) fn hot_start(&self) -> usize {
        hot_start_tiers(&self.cold, &self.warm)
    }
}

/// The cold-tier surface the streaming walk needs from
/// [`ColdTier`](crate::ColdTier), which [`read_segment_tiers`] reads through.
pub(crate) trait ColdSegments {
    fn line_count(&self) -> usize;
    fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, ScrollbackError>;
    fn segment_len_at(&self, idx: usize) -> usize;
}

impl ColdSegments for crate::ColdTier {
    fn line_count(&self) -> usize {
        crate::ColdTier::line_count(self)
    }
    fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, ScrollbackError> {
        crate::ColdTier::take_lines_from(self, idx)
    }
    fn segment_len_at(&self, idx: usize) -> usize {
        crate::ColdTier::segment_len_at(self, idx)
    }
}

/// First logical index of the hot tier: `cold + warm` lines.
pub(crate) fn hot_start_tiers(cold: &impl ColdSegments, warm: &crate::WarmTier) -> usize {
    cold.line_count().saturating_add(warm.line_count())
}

/// Bulk read for the streaming iterators over one cold/warm/hot tier stack:
/// owned lines from `idx` through the end of its tier segment (cold page /
/// warm block / one hot line), or the error plus the lines to skip.
// Skip: the tier-dispatch driver; each per-tier bulk read is classified on its
// own.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn read_segment_tiers(
    cold: &impl ColdSegments,
    warm: &crate::WarmTier,
    hot: &crate::HotTier,
    idx: usize,
) -> SegmentResult {
    let cold_count = cold.line_count();
    let warm_count = warm.line_count();

    if idx < cold_count {
        cold.take_lines_from(idx).map_err(|e| {
            // Skip the rest of the undecodable page, clamped into the
            // cold range; `max(1)` guarantees forward progress even
            // against a degenerate segment length.
            let skip = cold
                .segment_len_at(idx)
                .min(cold_count.saturating_sub(idx))
                .max(1);
            (e, skip)
        })
    } else if idx < cold_count.saturating_add(warm_count) {
        let warm_idx = idx.saturating_sub(cold_count);
        warm.take_lines_from(warm_idx).map_err(|e| {
            let skip = warm
                .segment_len_at(warm_idx)
                .min(warm_count.saturating_sub(warm_idx))
                .max(1);
            (e, skip)
        })
    } else {
        let hot_idx = idx.saturating_sub(cold_count).saturating_sub(warm_count);
        match hot.get(hot_idx) {
            Some(line) => Ok(vec![line.clone()]),
            // In-range index with no hot line (stale count): end of data.
            None => Ok(Vec::new()),
        }
    }
}

impl ScrollbackStorage {
    /// Dense forward walk from logical line `start` (0 = oldest) to the end:
    /// exactly one item per logical line, `None` for a line that cannot be read.
    ///
    /// The search index and its refresh key lines by absolute row, so a walk that
    /// skips a corrupt segment (as [`iter`](Self::iter) does) would shift every
    /// later line onto its neighbour's row. Item `k` is always line `start + k` and
    /// equals `get_line(start + k).ok().flatten()` (conformance tests in
    /// `storage_tests.rs`).
    ///
    /// One decode per warm block / cold page (lines moved out) and a
    /// `Cow::Borrowed` per hot line; a corrupt segment is logged once.
    #[must_use]
    pub fn dense_from(&self, start: usize) -> DenseScrollbackIter<'_> {
        let end = self.line_count();
        let hot_start = match self {
            ScrollbackStorage::Memory(sb) => sb.hot_start(),
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
    /// Logical index of the next item.
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

    // Skip: the segment-walk driver, like `ScrollbackIter::next`. Conformance-
    // tested against the per-line oracle.
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
            // Warm/cold: one decode for the rest of the segment, clamped to the hot
            // boundary so a malformed decode cannot push a line onto another's row.
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
/// Each warm block / cold page is decoded once and its lines moved out. Lines of
/// a corrupt segment are skipped with one warning per segment; the test-only
/// `skipped_lines` counts them.
pub struct ScrollbackIter<'a> {
    scrollback: &'a Scrollback,
    idx: usize,
    skipped_lines: usize,
    /// Decoded lines of the current segment, drained front-to-back. Owned:
    /// yielding is a move, never a clone.
    buf: VecDeque<Line>,
}

impl ScrollbackIter<'_> {
    /// Lines skipped due to decompression errors. Non-zero means the iterator
    /// yielded fewer items than `line_count()`.
    #[must_use]
    #[cfg(test)]
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }
}

impl Iterator for ScrollbackIter<'_> {
    type Item = Line;

    // Skip: the segment-walk driver; its bulk reads route into the per-tier
    // decode paths, each classified on its own. Round-trip and oracle tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.buf.pop_front() {
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
                        // In-range index with no data (stale count or short
                        // decode): end of data.
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
        // `idx` counts consumed lines, so `remaining` already includes the
        // buffered tail.
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
