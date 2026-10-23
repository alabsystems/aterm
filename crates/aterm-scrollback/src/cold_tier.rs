// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! In-memory cold tier.

use super::line::{Line, deserialize_page_lines};
use super::tier::WarmBlock;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;

/// Decompressed-page cache slots, as the warm tier's `CACHE_SLOTS`: a viewport
/// spans at most two pages, and a frame reads them in ascending order, so one
/// slot would miss on both pages every frame.
const CACHE_SLOTS: usize = 2;

/// The decompressed-page cache: one `(page_index, lines)` pair per slot, `None`
/// while the slot is cold (named for `clippy::type_complexity`).
type PageCache = RefCell<[Option<(usize, Vec<Line>)>; CACHE_SLOTS]>;

#[cfg(test)]
thread_local! {
    static COLD_FIND_PAGE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_cold_find_page_steps(steps: usize) {
    COLD_FIND_PAGE_STEPS.with(|c| c.set(c.get() + steps));
}

#[cfg(test)]
fn take_cold_find_page_steps() -> usize {
    COLD_FIND_PAGE_STEPS.with(|c| {
        let value = c.get();
        c.set(0);
        value
    })
}

/// A cold page (compressed with the LZ4 cold codec).
#[derive(Debug, Clone)]
struct ColdPage {
    /// Cold-codec compressed data (re-compressed from the LZ4 warm block).
    compressed: Vec<u8>,
    /// Number of lines in page.
    line_count: usize,
    /// Its rows are blank stand-ins for text already counted as lost: the page
    /// a quarantined warm block leaves in its place ([`ColdTier::push_lost_block`]).
    lost: bool,
}

impl ColdPage {
    /// Create a cold page from a warm block.
    fn from_warm_block(block: &WarmBlock) -> Result<Self, super::ScrollbackError> {
        let (compressed, line_count) = block.to_cold_compressed()?;

        Ok(Self {
            compressed,
            line_count,
            lost: false,
        })
    }

    /// Decompress and get all lines.
    fn decompress(&self) -> Result<Vec<Line>, super::ScrollbackError> {
        let decompressed = super::decode_cold_bounded(&self.compressed)?;
        Ok(deserialize_page_lines(&decompressed))
    }
}

/// What a byte-budget eviction ([`ColdTier::evict_bytes`]) took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Evicted {
    /// Logical lines removed.
    pub(crate) lines: usize,
    /// Of those, the rows of lost pages.
    pub(crate) already_lost: usize,
}

/// Cold tier: LZ4 compressed pages in memory.
#[derive(Debug)]
pub(crate) struct ColdTier {
    /// Compressed pages (VecDeque for O(1) pop_front during eviction).
    pages: VecDeque<ColdPage>,
    /// Total available line count (excludes consumed lines from front_offset).
    line_count: usize,
    /// Cumulative *physical* line counts, absolute since the last full rebuild:
    /// live entry `i` (`cumulative_lines[cum_start + i]`, page `i` of `pages`)
    /// holds `cumulative_base` + physical lines in pages `0..=i`.
    cumulative_lines: Vec<usize>,
    /// Dead-prefix length of `cumulative_lines` (entries of dropped pages),
    /// reclaimed once it outgrows the live half (`drop_front_index_entries`).
    cum_start: usize,
    /// Absolute cumulative value at the current front: physical lines in pages
    /// dropped since the last full rebuild.
    cumulative_base: usize,
    /// Recently decompressed pages, filled round-robin ([`CACHE_SLOTS`]).
    last_page_cache: PageCache,
    /// Round-robin write cursor into `last_page_cache`.
    cache_next: Cell<usize>,
    /// Running total for `compressed_size()`.
    bytes_used: usize,
    /// Lines logically consumed from the first page.
    front_offset: usize,
}

impl ColdTier {
    /// Create a new cold tier.
    #[must_use]
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            pages: VecDeque::new(),
            line_count: 0,
            cumulative_lines: Vec::new(),
            cum_start: 0,
            cumulative_base: 0,
            last_page_cache: RefCell::new([const { None }; CACHE_SLOTS]),
            cache_next: Cell::new(0),
            bytes_used: 0,
            front_offset: 0,
        }
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub(crate) const fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of pages.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Push a warm block (re-compresses with the cold codec).
    pub(crate) fn push_block(&mut self, block: &WarmBlock) -> usize {
        self.push(block, false)
    }

    /// Push `block` as the blank stand-in for a quarantined warm block's lines, whose loss is already counted.
    pub(crate) fn push_lost_block(&mut self, block: &WarmBlock) -> usize {
        self.push(block, true)
    }

    fn push(&mut self, block: &WarmBlock, lost: bool) -> usize {
        match ColdPage::from_warm_block(block) {
            Ok(mut page) => {
                page.lost = lost;
                let accepted = page.line_count;
                self.bytes_used = self.bytes_used.saturating_add(page.compressed.len());
                self.line_count = self.line_count.saturating_add(accepted);
                let cumulative = self
                    .cumulative_lines
                    .last()
                    .copied()
                    .unwrap_or(self.cumulative_base)
                    .saturating_add(accepted);
                self.cumulative_lines.push(cumulative);
                self.pages.push_back(page);
                accepted
            }
            Err(e) => {
                aterm_log::warn!("cold_tier::push_block: dropping block due to error: {e}");
                0
            }
        }
    }

    /// Get a line by index (0 = oldest available line, after `front_offset`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        let physical_idx = idx.saturating_add(self.front_offset);

        let Some((page_idx, line_in_page)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing cold page"
                ),
            )));
        };

        {
            let cache = self.last_page_cache.borrow();
            if let Some((_, lines)) = cache
                .iter()
                .flatten()
                .find(|(cached_idx, _)| *cached_idx == page_idx)
            {
                let Some(line) = lines.get(line_in_page).cloned() else {
                    return Err(super::ScrollbackError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "cold page {page_idx} missing line offset {line_in_page} for index {idx}"
                        ),
                    )));
                };
                return Ok(Some(line));
            }
        }

        let Some(page) = self.pages.get(page_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cold page index {page_idx} out of range"),
            )));
        };
        let lines = page.decompress()?;
        let Some(line) = lines.get(line_in_page).cloned() else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "cold page {page_idx} missing line offset {line_in_page} after decompression"
                ),
            )));
        };
        self.cache_page(page_idx, lines);
        Ok(Some(line))
    }

    /// Resolve a physical line index to `(page_idx, line_in_page)`: the one
    /// home of the cumulative-index geometry, shared by `get_line` and the bulk
    /// path (`take_lines_from`/`segment_len_at`).
    #[cfg_attr(trust_verify, trust::skip)]
    fn locate(&self, physical_idx: usize) -> Option<(usize, usize)> {
        let page_idx = self.find_page(physical_idx)?;
        let page_start = if page_idx == 0 {
            self.cumulative_base
        } else {
            self.live_cumulative()
                .get(page_idx.saturating_sub(1))
                .copied()
                .unwrap_or(self.cumulative_base)
        };
        Some((
            page_idx,
            physical_idx
                .saturating_add(self.cumulative_base)
                .saturating_sub(page_start),
        ))
    }

    /// Decode the page containing logical line `idx` and return owned lines from `idx` through the end of that page.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(Vec::new());
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((page_idx, line_in_page)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing cold page"
                ),
            )));
        };
        let Some(page) = self.pages.get(page_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cold page index {page_idx} out of range"),
            )));
        };
        let mut lines = page.decompress()?;
        if line_in_page == 0 {
            return Ok(lines);
        }
        let split_at = line_in_page.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` through the end of its page.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn segment_len_at(&self, idx: usize) -> usize {
        if idx >= self.line_count {
            return 0;
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((page_idx, _)) = self.locate(physical_idx) else {
            return 0;
        };
        let page_end = self
            .live_cumulative()
            .get(page_idx)
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_sub(self.cumulative_base);
        page_end
            .saturating_sub(self.front_offset)
            .min(self.line_count)
            .saturating_sub(idx)
    }

    /// Insert a decompressed page into the next round-robin cache slot.
    fn cache_page(&self, page_idx: usize, lines: Vec<Line>) {
        let mut cache = self.last_page_cache.borrow_mut();
        let slot = self.cache_next.get() % CACHE_SLOTS;
        if let Some(entry) = cache.get_mut(slot) {
            *entry = Some((page_idx, lines));
        }
        self.cache_next.set(slot.saturating_add(1) % CACHE_SLOTS);
    }

    /// Drop every cached page.
    fn clear_cache(&self) {
        *self.last_page_cache.borrow_mut() = [const { None }; CACHE_SLOTS];
        self.cache_next.set(0);
    }

    /// Live (non-dropped) region of the cumulative index. Total `get` keeps
    /// it panic-free; `cum_start <= len` is a maintained invariant.
    #[inline]
    fn live_cumulative(&self) -> &[usize] {
        self.cumulative_lines.get(self.cum_start..).unwrap_or(&[])
    }

    /// Drop the first `k` live cumulative entries in amortized O(k): advance
    /// `cum_start` and `cumulative_base`, leaving the absolute values
    /// untouched.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop_front_index_entries(&mut self, k: usize) {
        if k == 0 {
            return;
        }
        let live_len = self.live_cumulative().len();
        if k >= live_len {
            self.cumulative_lines.clear();
            self.cum_start = 0;
            self.cumulative_base = 0;
            return;
        }
        let last_dropped = self.cum_start.saturating_add(k).saturating_sub(1);
        self.cumulative_base = self
            .cumulative_lines
            .get(last_dropped)
            .copied()
            .unwrap_or(self.cumulative_base);
        self.cum_start = self.cum_start.saturating_add(k);
        if self.cum_start > self.cumulative_lines.len().saturating_sub(self.cum_start) {
            let dead = self.cum_start;
            self.cumulative_lines.drain(..dead);
            self.cum_start = 0;
        }
    }

    /// Find the page containing the given line index via binary search.
    fn find_page(&self, line_idx: usize) -> Option<usize> {
        let mut steps = 0usize;
        let target = line_idx
            .saturating_add(self.cumulative_base)
            .saturating_add(1);
        let live = self.live_cumulative();
        let result = super::binary_search_counted(live, target, &mut steps);
        #[cfg(test)]
        count_cold_find_page_steps(steps);
        match result {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < live.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        }
    }

    /// Remove the oldest `k` pages in a single batch.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn pop_front_batch(&mut self, k: usize) -> usize {
        if k == 0 || self.pages.is_empty() {
            return 0;
        }
        let k = k.min(self.pages.len());

        let mut evicted_lines = 0usize;
        let mut evicted_bytes = 0usize;
        for (i, page) in self.pages.drain(..k).enumerate() {
            let logical = if i == 0 {
                page.line_count.saturating_sub(self.front_offset)
            } else {
                page.line_count
            };
            evicted_lines = evicted_lines.saturating_add(logical);
            evicted_bytes = evicted_bytes.saturating_add(page.compressed.len());
        }
        self.bytes_used = self.bytes_used.saturating_sub(evicted_bytes);
        self.line_count = self.line_count.saturating_sub(evicted_lines);
        self.front_offset = 0;

        self.drop_front_index_entries(k);
        self.clear_cache();

        evicted_lines
    }

    /// Evict oldest pages until at least `target_bytes` of compressed memory is freed.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn evict_bytes(&mut self, target_bytes: usize) -> Evicted {
        if target_bytes == 0 || self.pages.is_empty() {
            return Evicted::default();
        }

        let mut bytes_freed = 0usize;
        let mut pages_to_evict = 0usize;
        let mut already_lost = 0usize;
        for page in &self.pages {
            if bytes_freed >= target_bytes {
                break;
            }
            if page.lost {
                let logical = if pages_to_evict == 0 {
                    page.line_count.saturating_sub(self.front_offset)
                } else {
                    page.line_count
                };
                already_lost = already_lost.saturating_add(logical);
            }
            bytes_freed = bytes_freed.saturating_add(page.compressed.len());
            pages_to_evict = pages_to_evict.saturating_add(1);
        }

        Evicted {
            lines: self.pop_front_batch(pages_to_evict),
            already_lost,
        }
    }

    /// Remove the oldest `n` logical lines from the front of the cold tier.
    ///
    /// # Panics
    /// Debug-asserts `n <= self.line_count` under `cfg(test)`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_front_lines(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            let mut msg = String::from("truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        let mut pages_dropped = 0usize;
        while let Some(front) = self.pages.front() {
            if self.front_offset >= front.line_count {
                let Some(page) = self.pages.pop_front() else {
                    break;
                };
                self.front_offset = self.front_offset.saturating_sub(page.line_count);
                self.bytes_used = self.bytes_used.saturating_sub(page.compressed.len());
                pages_dropped = pages_dropped.saturating_add(1);
            } else {
                break;
            }
        }

        if pages_dropped > 0 {
            self.drop_front_index_entries(pages_dropped);
            self.clear_cache();
        }
    }

    /// Clear all pages.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.line_count = 0;
        self.cumulative_lines.clear();
        self.cum_start = 0;
        self.cumulative_base = 0;
        self.clear_cache();
        self.bytes_used = 0;
        self.front_offset = 0;
    }

    /// Get total compressed size (for stats).
    #[must_use]
    #[inline]
    pub(crate) const fn compressed_size(&self) -> usize {
        self.bytes_used
    }

    /// Recompute the compressed size from the pages (test-only invariant check).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn recompute_compressed_size(&self) -> usize {
        let mut total = 0usize;
        for p in &self.pages {
            total = total.saturating_add(p.compressed.len());
        }
        total
    }
}

impl Default for ColdTier {
    fn default() -> Self {
        Self::new()
    }
}

#[path = "cold_tier_back_removal.rs"]
mod back_removal;

#[cfg(test)]
#[path = "cold_tier_tests.rs"]
mod tests;
