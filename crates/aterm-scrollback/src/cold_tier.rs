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
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;

/// Decompressed-page cache slots — the cold twin of the warm tier's
/// `CACHE_SLOTS`, and for the same reason.
///
/// TWO, not one. A render frame walks the visible rows top-to-bottom, which
/// maps to ASCENDING scrollback indices, so a viewport straddling a page
/// boundary used to thrash a single-slot cache: miss on page P (the slot held
/// P+1 from the previous frame) → full decode + page deserialization, cross the
/// boundary → miss on P+1 → a second full decode, frame ends holding P+1 → the
/// next frame repeats it exactly. A viewport spans at most two pages, so two
/// slots turn that steady state into hits.
const CACHE_SLOTS: usize = 2;

/// The decompressed-page cache itself: one `(page_index, lines)` pair per slot,
/// `None` while the slot is cold. Named because the inline spelling trips
/// `clippy::type_complexity`, which this crate denies via `clippy::all`.
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
    /// Cumulative *physical* line counts, kept ABSOLUTE since the last full
    /// rebuild: live entry `i` (`cumulative_lines[cum_start + i]`, page `i` of
    /// `pages`) holds `cumulative_base` + total physical lines in pages
    /// `0..=i`. Front drops advance `cum_start`/`cumulative_base` in
    /// O(dropped) instead of draining and rebasing every surviving entry —
    /// the old path memmoved AND rewrote the whole vector, O(total pages) per
    /// drop, on the push_line rotation hot path. Lookups add the base to the
    /// search target instead of ever touching stored values.
    /// Unchanged by front_offset — get_line adjusts indices before lookup.
    cumulative_lines: Vec<usize>,
    /// Dead-prefix length of `cumulative_lines` (entries of dropped pages).
    /// Reclaimed by one amortized memmove when it outgrows the live half
    /// (see `drop_front_index_entries`).
    cum_start: usize,
    /// Absolute cumulative value at the current front: total physical lines
    /// in pages dropped since the last full rebuild. Monotonic between
    /// rebuilds and bounded by lines ever stored in this process, so the
    /// saturating bumps are exact on every real path (crate idiom).
    cumulative_base: usize,
    /// Cache of recently decompressed pages: `(page_index, lines)` per slot,
    /// filled round-robin. See [`CACHE_SLOTS`] for why there is more than one.
    last_page_cache: PageCache,
    /// Round-robin write cursor into `last_page_cache`.
    cache_next: Cell<usize>,
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
                // `last()` is the last LIVE entry (the dead prefix sits at
                // the front); an empty vector means no pages survive, where
                // the invariant `empty => cum_start == 0 && base == 0` holds
                // (drop_front_index_entries/clear enforce it) — but the base
                // is the correct absolute floor either way.
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

        let Some((page_idx, line_in_page)) = self.locate(physical_idx) else {
            return Err(super::ScrollbackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "in-range line index {idx} (physical {physical_idx}) has no backing cold page"
                ),
            )));
        };

        // Check cache first — scan every slot; hit semantics are unchanged, a
        // straddling viewport just stops evicting the page it is about to read
        // again.
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

    /// Resolve a PHYSICAL line index to `(page_idx, line_in_page)`.
    ///
    /// The single home for the cumulative-index geometry, shared by the
    /// random-access read path (`get_line`) and the block-streaming bulk
    /// path (`take_lines_from`/`segment_len_at`) so the two cannot drift.
    // Skip: same guarded-index class as `get_line`, which this was factored
    // from. total `get` + saturating: `find_page` returned an in-range index
    // and `page_start <= physical_idx` by construction; the verifier cannot
    // chain either fact. The unreachable arms yield the same values.
    #[cfg_attr(trust_verify, trust::skip)]
    fn locate(&self, physical_idx: usize) -> Option<(usize, usize)> {
        let page_idx = self.find_page(physical_idx)?;
        // ABSOLUTE page start (ST-3 base-offset index): the dropped-prefix
        // base for live page 0, else the previous live entry's absolute
        // value; the base rides the query instead of rebasing stored values.
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

    /// Decode the page containing logical line `idx` and return OWNED lines
    /// from `idx` through the end of that page — the bulk-walk primitive
    /// (ST-6). One decode + one `split_off` per page: no per-line binary
    /// search, no per-line `Line` clone, and NO touch of the render-path
    /// page cache (a full-history walk must not evict the viewport's two
    /// hot slots).
    ///
    /// Returns an empty vec for an out-of-bounds `idx`.
    // Skip: the page lookup + decode path — same class as `get_line`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn take_lines_from(
        &self,
        idx: usize,
    ) -> Result<Vec<Line>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(Vec::new());
        }
        // Saturating: bounded by the tier's line totals — exact on every
        // real path (see get_line).
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
        // `min` keeps split_off total; a short decode yields a short
        // (possibly empty) segment, which the streaming iterator treats as
        // end-of-data — fail-closed, never a panic.
        let split_at = line_in_page.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` through the end of its containing page —
    /// how far a bulk walk skips when that page fails to decode. Zero when
    /// out of bounds. Never decodes.
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
        // ABSOLUTE live entry minus the dropped-prefix base (ST-3): the
        // physical end relative to the live front, exactly what the
        // pre-ST-3 relative entry held.
        let page_end = self
            .live_cumulative()
            .get(page_idx)
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_sub(self.cumulative_base);
        // Physical page end -> logical, clamped by the tier's logical count
        // (the last page's physical end IS the logical end), minus `idx`.
        page_end
            .saturating_sub(self.front_offset)
            .min(self.line_count)
            .saturating_sub(idx)
    }

    /// Insert a decompressed page into the next round-robin cache slot.
    fn cache_page(&self, page_idx: usize, lines: Vec<Line>) {
        let mut cache = self.last_page_cache.borrow_mut();
        let slot = self.cache_next.get() % CACHE_SLOTS;
        // `slot < CACHE_SLOTS` by construction, so the `else` arm is
        // unreachable; skipping the fill there is a no-op under that invariant
        // (the cache is a pure memo — a missed fill only costs a later
        // decompression) and keeps the write index-panic-free.
        if let Some(entry) = cache.get_mut(slot) {
            *entry = Some((page_idx, lines));
        }
        // Saturating: `slot` is already < CACHE_SLOTS, so this cannot overflow
        // on any real path; the modulo keeps the cursor in range regardless.
        self.cache_next.set(slot.saturating_add(1) % CACHE_SLOTS);
    }

    /// Drop EVERY cached page.
    ///
    /// Clear-all is an invariant, not laziness: every mutation that reaches
    /// here renumbers pages (`pop_front`, `pop_front_batch`, byte eviction,
    /// back-truncation), so a surviving entry keyed by its old index would
    /// serve the WRONG scrollback lines. Never make this selective — and route
    /// every invalidation site through this one helper.
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

    /// Drop the first `k` LIVE cumulative entries in O(k) amortized: advance
    /// `cum_start` and `cumulative_base`, leaving every surviving value
    /// untouched (they are absolute). The dead prefix is reclaimed by a
    /// single memmove only once it outgrows the live half — amortized O(1)
    /// per dropped entry, single-call bound O(live) word-moves — replacing
    /// the old unconditional drain-and-rebase, which cost O(total pages) of
    /// memmove PLUS rewrite on every drop reached from the push_line
    /// rotation path. Clears everything (and resets the base) when no live
    /// entry survives, so `empty => base == 0` holds for push_block.
    // Skip: `Vec::drain` under its guard — the BLANKET-unmodeled drain class
    // (guards don't chain). Same audit as `truncate_front_lines`.
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
        // New base = absolute value of the LAST dropped entry. `get` keeps
        // the lookup total; the None arm is unreachable (1 <= k < live_len
        // just established, and `cum_start + k - 1 < len` follows) and falls
        // back to the current base — it cannot execute on any real path.
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
        // ABSOLUTE target: stored entries are never rebased on front drops,
        // so the dropped-prefix base is added to the query instead. The
        // search runs over the LIVE suffix, whose indices are exactly the
        // live page indices.
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

        // O(1) amortized index maintenance: cursor + base advance; surviving
        // entries stay absolute and untouched (see drop_front_index_entries).
        self.drop_front_index_entries(1);

        // Invalidate cache — page indices shifted.
        self.clear_cache();

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

        // O(k) amortized index maintenance: cursor + base advance; surviving
        // entries stay absolute and untouched (see drop_front_index_entries).
        self.drop_front_index_entries(k);

        // Invalidate cache — page indices shifted.
        self.clear_cache();

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
            // O(pages_dropped) amortized: cursor + base advance; no surviving
            // entry is drained or rebased (see drop_front_index_entries — the
            // old path here cost O(total pages) memmove + rewrite per drop,
            // on the line-limit enforcement path of every push).
            self.drop_front_index_entries(pages_dropped);
            // Invalidate cache — live page indices shifted.
            self.clear_cache();
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
        self.cum_start = 0;
        self.cumulative_base = 0;
        self.clear_cache();
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
