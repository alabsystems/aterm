// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Forward iterators over scrollback lines.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Line, Scrollback, ScrollbackError, ScrollbackStorage};

/// One bulk read for the streaming walk.
pub(crate) type SegmentResult = Result<Vec<Line>, (ScrollbackError, usize)>;

impl Scrollback {
    /// Iterate over all lines (oldest to newest).
    #[must_use]
    pub const fn iter(&self) -> ScrollbackIter<'_> {
        ScrollbackIter {
            scrollback: self,
            idx: 0,
            skipped_lines: 0,
            buf: VecDeque::new(),
        }
    }

    /// Bulk read for the streaming iterators.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn read_segment(&self, idx: usize) -> SegmentResult {
        read_segment_tiers(&self.cold, &self.warm, &self.hot, idx)
    }

    /// First logical index of the hot tier (`cold + warm` lines).
    pub(crate) fn hot_start(&self) -> usize {
        hot_start_tiers(&self.cold, &self.warm)
    }
}

/// The cold-tier surface the streaming walk needs from [`ColdTier`](crate::ColdTier), which
/// [`read_segment_tiers`] reads through.
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

/// Bulk read for the streaming iterators over one cold/warm/hot tier stack.
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
            None => Ok(Vec::new()),
        }
    }
}

impl ScrollbackStorage {
    /// Dense forward walk from logical line `start` (0 = oldest) to the end.
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

/// Dense forward walk over a [`ScrollbackStorage`] — see [`ScrollbackStorage::dense_from`].
pub struct DenseScrollbackIter<'a> {
    storage: &'a ScrollbackStorage,
    idx: usize,
    end: usize,
    hot_start: usize,
    buf: VecDeque<Line>,
    owed_placeholders: usize,
    placeholders: usize,
}

impl DenseScrollbackIter<'_> {
    /// How many placeholders (unreadable lines) the walk has yielded so far.
    #[must_use]
    pub const fn placeholders(&self) -> usize {
        self.placeholders
    }

    const fn placeholder<'a>(&mut self) -> Option<Option<Cow<'a, Line>>> {
        self.idx = self.idx.saturating_add(1);
        self.placeholders = self.placeholders.saturating_add(1);
        Some(None)
    }
}

impl<'a> Iterator for DenseScrollbackIter<'a> {
    type Item = Option<Cow<'a, Line>>;

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
            let tier_left = self.hot_start.saturating_sub(self.idx);
            match self.storage.read_segment(self.idx) {
                Ok(mut lines) if !lines.is_empty() => {
                    lines.truncate(tier_left);
                    self.buf = VecDeque::from(lines);
                }
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
pub struct ScrollbackIter<'a> {
    scrollback: &'a Scrollback,
    idx: usize,
    skipped_lines: usize,
    buf: VecDeque<Line>,
}

impl ScrollbackIter<'_> {
    /// Lines skipped due to decompression errors.
    #[must_use]
    #[cfg(test)]
    pub const fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }
}

impl Iterator for ScrollbackIter<'_> {
    type Item = Line;

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
                        return None;
                    }
                    self.buf = VecDeque::from(lines);
                }
                Err((e, skip)) => {
                    aterm_log::warn!(
                        "scrollback iter: skipping {skip} line(s) at {}: {e}",
                        self.idx
                    );
                    let skip = skip.min(total.saturating_sub(self.idx)).max(1);
                    self.skipped_lines = self.skipped_lines.saturating_add(skip);
                    self.idx = self.idx.saturating_add(skip);
                }
            }
        }
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
