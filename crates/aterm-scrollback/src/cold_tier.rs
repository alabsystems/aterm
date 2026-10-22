// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! In-memory cold tier: compressed pages.
//!
//! Pages are compressed with the cold codec: the warm tier's size-prepended
//! LZ4 (`encode_cold_block`), the same in every build.

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
///
/// Stored in memory.
#[derive(Debug, Clone)]
struct ColdPage {
    /// Cold-codec compressed data (re-compressed from the LZ4 warm block).
    compressed: Vec<u8>,
    /// Number of lines in page.
    line_count: usize,
    /// Its rows are blank stand-ins for text already counted as lost: the page
    /// a quarantined warm block leaves in its place ([`ColdTier::push_lost_block`]).
    /// Evicting it loses nothing more ([`Evicted::already_lost`]).
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
    /// Of those, the rows of lost pages: blank stand-ins for text the loss
    /// counter already holds, which their eviction must not count again.
    pub(crate) already_lost: usize,
}

/// Cold tier: LZ4 compressed pages (in-memory).
///
/// Uses cumulative line counts for O(log P) page lookup and caches the
/// last decompressed page to avoid redundant decompression.
///
/// The `front_offset` field enables O(1) line-limit enforcement: instead of
/// decompressing the boundary page to remove a few oldest lines, we simply
/// advance the offset. The first page is dropped when fully consumed.
#[derive(Debug)]
pub(crate) struct ColdTier {
    /// Compressed pages (VecDeque for O(1) pop_front during eviction).
    pages: VecDeque<ColdPage>,
    /// Total available line count (excludes consumed lines from front_offset).
    line_count: usize,
    /// Cumulative *physical* line counts, absolute since the last full rebuild:
    /// live entry `i` (`cumulative_lines[cum_start + i]`, page `i` of `pages`)
    /// holds `cumulative_base` + physical lines in pages `0..=i`. Front drops
    /// advance `cum_start`/`cumulative_base` in O(dropped) instead of rebasing
    /// every entry; lookups add the base to the search target. Unchanged by
    /// `front_offset` (callers adjust indices before lookup).
    cumulative_lines: Vec<usize>,
    /// Dead-prefix length of `cumulative_lines` (entries of dropped pages),
    /// reclaimed once it outgrows the live half (`drop_front_index_entries`).
    cum_start: usize,
    /// Absolute cumulative value at the current front: physical lines in pages
    /// dropped since the last full rebuild. Bounded by lines ever stored, so
    /// its saturating bumps are exact.
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
    ///
    /// Returns the number of lines accepted. On a decompression or
    /// re-compression failure, logs a warning, drops the block and returns 0,
    /// so callers can adjust their line counts.
    pub(crate) fn push_block(&mut self, block: &WarmBlock) -> usize {
        self.push(block, false)
    }

    /// Push `block` as the blank stand-in for a quarantined warm block's
    /// lines, whose loss is already counted: its eviction reports them as
    /// [`Evicted::already_lost`]. Returns the lines accepted, as
    /// [`Self::push_block`].
    pub(crate) fn push_lost_block(&mut self, block: &WarmBlock) -> usize {
        self.push(block, true)
    }

    fn push(&mut self, block: &WarmBlock, lost: bool) -> usize {
        match ColdPage::from_warm_block(block) {
            Ok(mut page) => {
                page.lost = lost;
                let accepted = page.line_count;
                // Saturating: exact on every real path.
                self.bytes_used = self.bytes_used.saturating_add(page.compressed.len());
                self.line_count = self.line_count.saturating_add(accepted);
                // `last()` is the last live entry (the dead prefix is at the
                // front); when empty, the base is the absolute floor.
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
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for decompression failures.
    // Skip: typed-TrustIr lowering does not complete for this body (a toolchain
    // gap).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        // Logical (0 = oldest available) to physical (0 = first line of the
        // first page, consumed lines included). Saturating: exact.
        let physical_idx = idx.saturating_add(self.front_offset);

        let Some((page_idx, line_in_page)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing cold page"
                ),
            )));
        };

        // Check every cache slot first.
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

        // Decompress and cache.
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
    // Skip: guarded-index class. `find_page` returned an in-range index and
    // `page_start <= physical_idx` by construction; the verifier cannot chain
    // either fact.
    #[cfg_attr(trust_verify, trust::skip)]
    fn locate(&self, physical_idx: usize) -> Option<(usize, usize)> {
        let page_idx = self.find_page(physical_idx)?;
        // Absolute page start: the dropped-prefix base for live page 0, else
        // the previous live entry.
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

    /// Decode the page containing logical line `idx` and return owned lines
    /// from `idx` through the end of that page: the bulk-walk primitive. It
    /// never touches the page cache (a full-history walk must not evict the
    /// viewport's slots). Empty for an out-of-bounds `idx`.
    // Skip: same class as `get_line`.
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
        // A whole page is already the segment; skip split_off(0)'s allocation.
        if line_in_page == 0 {
            return Ok(lines);
        }
        // `min` keeps split_off total; a short decode yields a short segment,
        // which the streaming iterator treats as end-of-data.
        let split_at = line_in_page.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` through the end of its page: how far a bulk walk
    /// skips when that page fails to decode. Zero when out of bounds; never
    /// decodes.
    // Skip: same guarded-index class as `locate`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn segment_len_at(&self, idx: usize) -> usize {
        if idx >= self.line_count {
            return 0;
        }
        let physical_idx = idx.saturating_add(self.front_offset);
        let Some((page_idx, _)) = self.locate(physical_idx) else {
            return 0;
        };
        // Absolute live entry minus the base: the physical end relative to the
        // live front.
        let page_end = self
            .live_cumulative()
            .get(page_idx)
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_sub(self.cumulative_base);
        // Physical page end -> logical, clamped by the tier's logical count
        // (the last page's physical end is the logical end), minus `idx`.
        page_end
            .saturating_sub(self.front_offset)
            .min(self.line_count)
            .saturating_sub(idx)
    }

    /// Insert a decompressed page into the next round-robin cache slot.
    fn cache_page(&self, page_idx: usize, lines: Vec<Line>) {
        let mut cache = self.last_page_cache.borrow_mut();
        let slot = self.cache_next.get() % CACHE_SLOTS;
        // `slot < CACHE_SLOTS`, so the `get_mut` always hits; it keeps the
        // write index-panic-free.
        if let Some(entry) = cache.get_mut(slot) {
            *entry = Some((page_idx, lines));
        }
        self.cache_next.set(slot.saturating_add(1) % CACHE_SLOTS);
    }

    /// Drop every cached page.
    ///
    /// Every mutation that reaches here renumbers pages, so an entry keyed by
    /// its old index would serve the wrong lines. Never make this selective,
    /// and route every invalidation through it.
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
    /// untouched. The dead prefix is reclaimed by one memmove once it outgrows
    /// the live half. When no live entry survives, everything resets, so
    /// `empty => base == 0`.
    // Skip: `Vec::drain` under its guard, the unmodeled drain class (guards
    // don't chain).
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
        // New base = absolute value of the last dropped entry; the `None` arm
        // is unreachable (`1 <= k < live_len`).
        let last_dropped = self.cum_start.saturating_add(k).saturating_sub(1);
        self.cumulative_base = self
            .cumulative_lines
            .get(last_dropped)
            .copied()
            .unwrap_or(self.cumulative_base);
        self.cum_start = self.cum_start.saturating_add(k);
        // Amortized reclamation of the dead prefix.
        if self.cum_start > self.cumulative_lines.len().saturating_sub(self.cum_start) {
            let dead = self.cum_start;
            self.cumulative_lines.drain(..dead);
            self.cum_start = 0;
        }
    }

    /// Find the page containing the given line index via binary search.
    fn find_page(&self, line_idx: usize) -> Option<usize> {
        // Plain `&mut usize` step counter (was a `fn()`/closure callback): the
        // Trust lane (`ship trust run`) treats an opaque callback as an absent callee that
        // may panic. The counter is diagnostic-only; tests fold it into the
        // same thread-local the old per-step callback incremented, so the
        // observable count is unchanged.
        let mut steps = 0usize;
        // Absolute target (stored entries are never rebased), searched over the
        // live suffix, whose indices are the live page indices. Saturating: a
        // saturated target still reads "not found".
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
    ///
    /// Returns total *logical* lines evicted (the first page's count is
    /// adjusted for `front_offset`). O(P), not the O(k*P) of k `pop_front()`s.
    // Skip: `drain(..)` (unmodeled; guards don't chain) and its guarded slice
    // read.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn pop_front_batch(&mut self, k: usize) -> usize {
        if k == 0 || self.pages.is_empty() {
            return 0;
        }
        let k = k.min(self.pages.len());

        // Drain evicted pages and sum their line counts.
        let mut evicted_lines = 0usize;
        let mut evicted_bytes = 0usize;
        for (i, page) in self.pages.drain(..k).enumerate() {
            let logical = if i == 0 {
                page.line_count.saturating_sub(self.front_offset)
            } else {
                page.line_count
            };
            // Saturating: bounded by `line_count` and `bytes_used`; exact.
            evicted_lines = evicted_lines.saturating_add(logical);
            evicted_bytes = evicted_bytes.saturating_add(page.compressed.len());
        }
        self.bytes_used = self.bytes_used.saturating_sub(evicted_bytes);
        self.line_count = self.line_count.saturating_sub(evicted_lines);
        self.front_offset = 0; // New front page starts fresh.

        self.drop_front_index_entries(k);
        self.clear_cache();

        evicted_lines
    }

    /// Evict oldest pages until at least `target_bytes` of compressed memory is freed.
    ///
    /// Returns the lines evicted, and how many of them were a lost page's
    /// already-counted rows. Counts pages to evict first, then batch-removes
    /// them in O(P) total instead of the O(k*P) cost of repeated `pop_front()`.
    // Skip: the eviction walk drives a std iterator whose `next` is an absent
    // body (generic trait path). Pure bookkeeping over owned blocks; the
    // budget contract is ARENA-SCROLL-exercised. Re-measured 2026-09-27 on seal 321aaeda7 with this skip removed:
    // INCONCLUSIVE (1 of 2 obligations unknown). Needed from $HOME/trust, in a
    // promoted seal: totality for std iterator `next`; then drop this.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn evict_bytes(&mut self, target_bytes: usize) -> Evicted {
        if target_bytes == 0 || self.pages.is_empty() {
            return Evicted::default();
        }

        // Count pages needed to free target_bytes, and the lost rows among
        // them (the first page's count adjusted for `front_offset`, as
        // `pop_front_batch` adjusts it).
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
                // Bounded by the lines in memory, like `pop_front_batch`'s sum.
                already_lost = already_lost.saturating_add(logical);
            }
            // `bytes_freed` sums the lengths of live `compressed`
            // allocations (bounded by self.bytes_used) and `pages_to_evict`
            // counts pages (bounded by self.pages.len()); both fit in usize,
            // so saturating_add never saturates and is behavior-identical.
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
    /// Advances `front_offset` by `n` and drops pages that become fully
    /// consumed, without decompressing: O(1), or O(pages dropped).
    ///
    /// # Panics
    ///
    /// Debug-asserts `n <= self.line_count` under `cfg(test)`; other builds warn
    /// and saturate.
    // Skip: guarded ring pops plus the drain behind `drop_front_index_entries`
    // (the unmodeled drain class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_front_lines(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        // Asserted under `cfg(test)` only: a contract assert is a reachable
        // panic the strict L0 gate cannot refute.
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            // Pre-composed message via the log shim: identical rendered
            // record, but no macro-expanded `format_args!` unsafe in THIS
            // function (which the Trust lane would escalate — see log_shim.rs).
            let mut msg = String::from("truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        // Saturating: `front_offset` and `n` are bounded by lines this process
        // actually stored, so the sum always fits in `usize` — the saturation
        // just discharges the Trust lane's unconstrained-input counterexample.
        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        // Drop fully consumed pages from the front.
        let mut pages_dropped = 0usize;
        while let Some(front) = self.pages.front() {
            if self.front_offset >= front.line_count {
                // `let-else` + break instead of `.expect(..)`: the loop
                // condition just witnessed a front page, so the `None` arm is
                // unreachable and behavior identical — but the Trust lane
                // must refute every reachable panic and cannot carry the
                // witness across the `pop_front` call.
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

    // Back-removal (truncate_back_lines) is in cold_tier_back_removal.rs.

    /// Clear all pages.
    // Skip: the page ring's element drop glue (std/alloc internals).
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
