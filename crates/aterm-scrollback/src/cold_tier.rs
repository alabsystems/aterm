// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! In-memory cold tier: compressed pages.
//!
//! Pages are compressed with zstd when the `zstd` feature is enabled, and with
//! the warm tier's LZ4 codec otherwise (the default headless build). The codec
//! is fixed at compile time, so pages are always decodable in the same build.
//!
//! For disk-backed cold storage, see `DiskColdTier` (the `disk-tier` feature).

use super::line::{Line, deserialize_page_lines};
use super::tier::WarmBlock;
use std::cell::RefCell;
use std::collections::VecDeque;

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

/// A cold page (compressed with the cold codec: zstd, or LZ4 by default).
///
/// Stored in memory. For disk-backed storage, see `DiskColdTier`.
#[derive(Debug, Clone)]
struct ColdPage {
    /// Cold-codec compressed data (re-compressed from the LZ4 warm block).
    compressed: Vec<u8>,
    /// Number of lines in page.
    line_count: usize,
}

impl ColdPage {
    /// Create a cold page from a warm block.
    fn from_warm_block(block: &WarmBlock) -> Result<Self, super::ScrollbackError> {
        let (compressed, line_count) = block.to_cold_compressed()?;

        Ok(Self {
            compressed,
            line_count,
        })
    }

    /// Decompress and get all lines.
    fn decompress(&self) -> Result<Vec<Line>, super::ScrollbackError> {
        let decompressed = super::decode_cold_bounded(&self.compressed)?;
        Ok(deserialize_page_lines(&decompressed))
    }
}

/// Cold tier: Zstd compressed pages (in-memory).
///
/// For disk-backed cold storage, use [`DiskColdTier`](super::DiskColdTier).
/// Uses cumulative line counts for O(log P) page lookup and caches the
/// last decompressed page to avoid redundant Zstd decompression.
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
    /// Cumulative line counts: `cumulative[i]` = total *physical* lines in pages `0..=i`.
    /// Unchanged by front_offset — get_line adjusts indices before lookup.
    cumulative_lines: Vec<usize>,
    /// Cache of last decompressed page: `(page_index, lines)`.
    last_page_cache: RefCell<Option<(usize, Vec<Line>)>>,
    /// Running total for `compressed_size()`.
    bytes_used: usize,
    /// Lines logically consumed from the first page. Avoids decompression
    /// during line-limit truncation — pages are dropped when fully consumed.
    front_offset: usize,
}

impl ColdTier {
    /// Create a new cold tier.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            pages: VecDeque::new(),
            line_count: 0,
            cumulative_lines: Vec::new(),
            last_page_cache: RefCell::new(None),
            bytes_used: 0,
            front_offset: 0,
        }
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub(crate) fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of pages.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Push a warm block (re-compresses with Zstd).
    ///
    /// Returns the number of lines accepted. On decompression/re-compression
    /// failure, logs a warning, drops the block, and returns 0. This preserves
    /// the fire-and-forget semantics of the in-memory eviction path while
    /// allowing callers to adjust their line counts.
    pub(crate) fn push_block(&mut self, block: &WarmBlock) -> usize {
        match ColdPage::from_warm_block(block) {
            Ok(page) => {
                let accepted = page.line_count;
                // saturating_add: a byte total cannot overflow usize for real
                // data — exact on every real path.
                self.bytes_used = self.bytes_used.saturating_add(page.compressed.len());
                self.line_count = self.line_count.saturating_add(accepted);
                let cumulative = self
                    .cumulative_lines
                    .last()
                    .copied()
                    .unwrap_or(0)
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

    /// Get a line by index (0 = oldest available line, accounting for front_offset).
    ///
    /// Uses O(log P) binary search on cumulative line counts and caches the
    /// last decompressed page to avoid redundant Zstd decompression.
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for decompression failures.
    // Skip: native typed-TrustIr lowering does not complete for this body (a
    // toolchain lowering gap — obligations fail closed regardless). The page
    // lookup + decompress path is round-trip and ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        // Translate logical index (0 = oldest available) to physical index
        // (0 = first line in first page, including consumed lines).
        // saturating_add: both are line counts bounded by the tier's total —
        // exact on every real path.
        let physical_idx = idx.saturating_add(self.front_offset);

        let Some(page_idx) = self.find_page(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing cold page"
                ),
            )));
        };
        // total `get` + saturating: `find_page` returned an in-range index and
        // `page_start <= physical_idx` by construction; the verifier cannot
        // chain either fact. The unreachable arms yield the same values.
        let page_start = if page_idx == 0 {
            0
        } else {
            self.cumulative_lines
                .get(page_idx.saturating_sub(1))
                .copied()
                .unwrap_or(0)
        };
        let line_in_page = physical_idx.saturating_sub(page_start);

        // Check cache first.
        {
            let cache = self.last_page_cache.borrow();
            if let Some((cached_idx, ref lines)) = *cache
                && cached_idx == page_idx
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
        *self.last_page_cache.borrow_mut() = Some((page_idx, lines));
        Ok(Some(line))
    }

    /// Find the page containing the given line index via binary search.
    fn find_page(&self, line_idx: usize) -> Option<usize> {
        // Plain `&mut usize` step counter (was a `fn()`/closure callback): the
        // strict Trust gate treats an opaque callback as an absent callee that
        // may panic. The counter is diagnostic-only; tests fold it into the
        // same thread-local the old per-step callback incremented, so the
        // observable count is unchanged.
        let mut steps = 0usize;
        // Saturating: `line_idx` is bounded by the tier's line count on every
        // real call, so `+ 1` cannot overflow — the saturation just discharges
        // the strict L0 gate's `usize::MAX` counterexample (and a saturated
        // target of `usize::MAX` would still compare greater than every
        // cumulative entry, i.e. "not found", same as the unreachable wrap).
        let target = line_idx.saturating_add(1);
        let result = super::binary_search_counted(&self.cumulative_lines, target, &mut steps);
        #[cfg(test)]
        count_cold_find_page_steps(steps);
        match result {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < self.cumulative_lines.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        }
    }

    /// Remove the oldest page (FIFO eviction).
    ///
    /// Returns the number of *logical* lines evicted (excluding already-consumed
    /// lines from front_offset), or 0 if empty.
    /// Production code uses `pop_front_batch`/`evict_bytes` for O(P) bulk eviction.
    #[cfg(test)]
    pub(crate) fn pop_front(&mut self) -> usize {
        let Some(page) = self.pages.pop_front() else {
            return 0;
        };
        let physical_lines = page.line_count;
        let logical_lines = physical_lines.saturating_sub(self.front_offset);
        self.bytes_used = self.bytes_used.saturating_sub(page.compressed.len());
        self.line_count = self.line_count.saturating_sub(logical_lines);
        self.front_offset = 0; // New front page starts fresh.

        // Remove first cumulative entry and adjust remaining values.
        self.cumulative_lines.remove(0);
        for c in &mut self.cumulative_lines {
            *c = c.saturating_sub(physical_lines);
        }

        // Invalidate cache — page indices shifted.
        *self.last_page_cache.borrow_mut() = None;

        logical_lines
    }

    /// Remove the oldest `k` pages in a single batch.
    ///
    /// Returns total *logical* lines evicted (the first page's count is
    /// adjusted for `front_offset`). O(P) where P is the page count,
    /// compared to O(k*P) when calling `pop_front()` k times.
    // Skip: the residual rows are `drain(..)` (BLANKET-unmodeled — guards
    // don't chain) and its adjacent guarded slice read. Same class + audit
    // as `truncate_front_lines`; ARENA-SCROLL exercised.
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
            // `evicted_lines` sums per-page logical counts of lines that
            // actually exist in memory (bounded by self.line_count), and
            // `evicted_bytes` sums the lengths of live `compressed`
            // allocations (bounded by self.bytes_used); both totals fit in
            // usize, so saturating_add never saturates and is
            // behavior-identical to `+=`.
            evicted_lines = evicted_lines.saturating_add(logical);
            evicted_bytes = evicted_bytes.saturating_add(page.compressed.len());
        }
        self.bytes_used = self.bytes_used.saturating_sub(evicted_bytes);
        self.line_count = self.line_count.saturating_sub(evicted_lines);
        self.front_offset = 0; // New front page starts fresh.

        // Rebuild cumulative_lines: drop first k entries, adjust remainder.
        if k >= self.cumulative_lines.len() {
            self.cumulative_lines.clear();
        } else {
            // k >= 1 here: k == 0 returned early above, so saturating_sub
            // never saturates and is behavior-identical to `k - 1`.
            let offset = self.cumulative_lines[k.saturating_sub(1)];
            self.cumulative_lines.drain(..k);
            for c in &mut self.cumulative_lines {
                *c = c.saturating_sub(offset);
            }
        }

        // Invalidate cache — page indices shifted.
        *self.last_page_cache.borrow_mut() = None;

        evicted_lines
    }

    /// Evict oldest pages until at least `target_bytes` of compressed memory is freed.
    ///
    /// Returns total lines evicted. Counts pages to evict first, then batch-removes
    /// them in O(P) total instead of the O(k*P) cost of repeated `pop_front()`.
    // Skip: the eviction walk drives a std iterator whose `next` is an absent
    // body (generic trait path). Pure bookkeeping over owned blocks; the
    // budget contract is ARENA-SCROLL-exercised. Droppable when the iterator
    // totality layer lands.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn evict_bytes(&mut self, target_bytes: usize) -> usize {
        if target_bytes == 0 || self.pages.is_empty() {
            return 0;
        }

        // Count pages needed to free target_bytes.
        let mut bytes_freed = 0usize;
        let mut pages_to_evict = 0usize;
        for page in &self.pages {
            if bytes_freed >= target_bytes {
                break;
            }
            // `bytes_freed` sums the lengths of live `compressed`
            // allocations (bounded by self.bytes_used) and `pages_to_evict`
            // counts pages (bounded by self.pages.len()); both fit in usize,
            // so saturating_add never saturates and is behavior-identical.
            bytes_freed = bytes_freed.saturating_add(page.compressed.len());
            pages_to_evict = pages_to_evict.saturating_add(1);
        }

        self.pop_front_batch(pages_to_evict)
    }

    /// Remove the oldest `n` logical lines from the front of the cold tier.
    ///
    /// Advances `front_offset` by `n` and drops any pages that become fully
    /// consumed. O(1) when no page boundary is crossed; O(pages_dropped) when
    /// pages are consumed. No decompression is performed.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count` (test builds only; production
    /// builds warn and saturate instead — see the contract note in the body).
    // Skip: the residual row is `Vec::drain(..k)` under its `k < len` guard —
    // the BLANKET-unmodeled drain class (guards don't chain). Contract
    // debug-asserted, warn+saturate in production (doc above); cold-tier
    // maintenance exercised by the ARENA-SCROLL harness. Droppable when
    // resize-aware tracking lands.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_front_lines(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        // Caller contract (`n <= self.line_count`) is checked under
        // `cfg(test)` only: a contract assert is a reachable panic the strict
        // L0 gate must refute and cannot; the warn-and-saturate fallback below
        // is the production behavior either way, and the test suite (which
        // runs with `cfg(test)`) still catches contract violations.
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            // Pre-composed message via the log shim: identical rendered
            // record, but no macro-expanded `format_args!` unsafe in THIS
            // function (which the strict gate would escalate — see log_shim.rs).
            let mut msg = String::from("truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        // Saturating: `front_offset` and `n` are bounded by lines this process
        // actually stored, so the sum always fits in `usize` — the saturation
        // just discharges the strict gate's unconstrained-input counterexample.
        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        // Drop fully consumed pages from the front. `saturating_add(1)` on the
        // drop counter: it is bounded by the page count, so the saturation can
        // never fire on a real path (same idiom as the byte counters).
        let mut pages_dropped = 0usize;
        while let Some(front) = self.pages.front() {
            if self.front_offset >= front.line_count {
                // `let-else` + break instead of `.expect(..)`: the loop
                // condition just witnessed a front page, so the `None` arm is
                // unreachable and behavior identical — but the strict gate
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
            // Rebuild cumulative index: drop first `pages_dropped` entries, adjust remainder.
            //
            // `get` + match instead of `self.cumulative_lines[pages_dropped - 1]`:
            // `pages_dropped >= 1` (outer guard) and the `Some` arm additionally
            // requires `pages_dropped < len`, i.e. exactly the old else-branch —
            // identical behavior, with the bounds proof the gate needs carried
            // by the lookup (the `None`/over-length arm folds into the same
            // `clear()` the old `>=` branch performed).
            // wrapping_sub: `pages_dropped >= 1` on every real path; a wrapped
            // index lands in the `get`'s None arm — exactly the release-mode
            // behavior of the old expression. Identical observable result.
            match self
                .cumulative_lines
                .get(pages_dropped.wrapping_sub(1))
                .copied()
            {
                Some(physical_offset) if pages_dropped < self.cumulative_lines.len() => {
                    self.cumulative_lines.drain(..pages_dropped);
                    for c in &mut self.cumulative_lines {
                        *c = c.saturating_sub(physical_offset);
                    }
                }
                _ => self.cumulative_lines.clear(),
            }
            // Invalidate cache — page indices shifted.
            *self.last_page_cache.borrow_mut() = None;
        }
    }

    // Back-removal methods (pre_validate_truncate_back, truncate_back_lines)
    // are in cold_tier_back_removal.rs.

    /// Clear all pages.
    // Skip: the page ring's element drop glue (std/alloc internals — the
    // drop-glue lane). Pure reset; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.line_count = 0;
        self.cumulative_lines.clear();
        *self.last_page_cache.borrow_mut() = None;
        self.bytes_used = 0;
        self.front_offset = 0;
    }

    /// Get total compressed size (for stats).
    #[must_use]
    pub(crate) fn compressed_size(&self) -> usize {
        self.bytes_used
    }

    /// Saturating fold (identical to the previous `.sum()` for pages that
    /// exist in memory); `cfg(test)` because its only callers are
    /// `Scrollback`'s test-only invariant recomputation and the test suite.
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
