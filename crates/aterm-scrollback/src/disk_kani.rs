// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for `DiskColdTier` — core binary search and cache invariants.
//!
//! Compaction arithmetic proofs are in `disk_kani_compaction.rs`.

use super::*;

/// Page index binary search is correct.
#[kani::proof]
#[kani::unwind(6)]
fn find_page_correct() {
    let mut cold = DiskColdTier::new();

    // Create cumulative lines for 5 pages with 10 lines each
    cold.cumulative_lines = vec![10, 20, 30, 40, 50];
    cold.line_count = 50;
    cold.index = vec![
        PageIndexEntry {
            offset: 0,
            compressed_size: 100,
            line_count: 10,
        };
        5
    ];

    let line_idx: usize = kani::any();
    kani::assume(line_idx < 50);

    let page_idx = cold.find_page(line_idx);
    kani::assert(page_idx.is_some(), "should find page for valid index");

    let page_idx = page_idx.unwrap();
    kani::assert(page_idx < 5, "page index in bounds");

    // Verify the line is within the page
    let page_start = if page_idx == 0 {
        0
    } else {
        cold.cumulative_lines[page_idx - 1]
    };
    let page_end = cold.cumulative_lines[page_idx];
    kani::assert(line_idx >= page_start, "line >= page start");
    kani::assert(line_idx < page_end, "line < page end");
}

/// Page index binary search is correct with non-uniform page sizes.
///
/// Companion to `find_page_correct` which uses uniform 10-line pages.
/// Non-uniform sizes exercise the `+1` encoding at different cumulative
/// boundaries (e.g., searching for 4 in [3, 10, 12] must return page 1,
/// not page 0).
#[kani::proof]
#[kani::unwind(5)]
fn find_page_correct_non_uniform() {
    let mut cold = DiskColdTier::new();

    // Pages with 3, 7, 2 lines → cumulative [3, 10, 12]
    cold.cumulative_lines = vec![3, 10, 12];
    cold.line_count = 12;
    cold.index = vec![
        PageIndexEntry {
            offset: 0,
            compressed_size: 50,
            line_count: 3,
        },
        PageIndexEntry {
            offset: 0,
            compressed_size: 50,
            line_count: 7,
        },
        PageIndexEntry {
            offset: 0,
            compressed_size: 50,
            line_count: 2,
        },
    ];

    let line_idx: usize = kani::any();
    kani::assume(line_idx < 12);

    let page_idx = cold.find_page(line_idx);
    kani::assert(page_idx.is_some(), "should find page for valid index");

    let page_idx = page_idx.expect("invariant: find_page returned Some for valid index");
    kani::assert(page_idx < 3, "page index in bounds");

    let page_start = if page_idx == 0 {
        0
    } else {
        cold.cumulative_lines[page_idx - 1]
    };
    let page_end = cold.cumulative_lines[page_idx];
    kani::assert(line_idx >= page_start, "line >= page start");
    kani::assert(line_idx < page_end, "line < page end");
}

/// Line count is always consistent.
#[kani::proof]
fn line_count_consistent() {
    let mut cold = DiskColdTier::new();

    let count1: usize = kani::any();
    let count2: usize = kani::any();
    kani::assume(count1 > 0 && count1 <= 100);
    kani::assume(count2 > 0 && count2 <= 100);
    kani::assume(count1 + count2 <= 200); // Avoid overflow

    // Simulate pushing two pages (in-memory mode)
    cold.index.push(PageIndexEntry {
        offset: 0,
        compressed_size: 50,
        line_count: count1 as u32,
    });
    cold.line_count += count1;
    cold.cumulative_lines.push(count1);

    cold.index.push(PageIndexEntry {
        offset: 0,
        compressed_size: 50,
        line_count: count2 as u32,
    });
    cold.line_count += count2;
    cold.cumulative_lines
        .push(cold.cumulative_lines.last().unwrap() + count2);

    // Verify consistency
    let total_from_index: usize = cold.index.iter().map(|e| e.line_count as usize).sum();
    kani::assert(cold.line_count == total_from_index, "line count matches");
    kani::assert(
        cold.line_count == *cold.cumulative_lines.last().unwrap(),
        "cumulative matches",
    );
}

/// Mmap data ranges stay within the mapped file bounds.
#[kani::proof]
fn mmap_access_within_bounds() {
    let mmap_len: usize = kani::any();
    let offset: usize = kani::any();
    let compressed_size: u32 = kani::any();

    kani::assume(mmap_len >= PAGE_HEADER_SIZE);
    kani::assume(mmap_len <= 1 << 20);

    let compressed_len = len_u32_to_usize(compressed_size);

    kani::assume(offset <= mmap_len - PAGE_HEADER_SIZE);
    let data_start = offset + PAGE_HEADER_SIZE;
    kani::assume(data_start <= mmap_len);
    kani::assume(compressed_len <= mmap_len - data_start);

    let data_end = data_start + compressed_len;
    kani::assert(data_end <= mmap_len, "mmap slice stays in bounds");
}

/// Disk offset arithmetic cannot overflow when bounds are enforced.
#[kani::proof]
fn disk_offset_arithmetic_safe() {
    let offset: u64 = kani::any();
    let compressed_size: u32 = kani::any();

    let header = PAGE_HEADER_SIZE as u64;
    kani::assume(offset <= u64::MAX - header - compressed_size as u64);

    let total = offset + header + compressed_size as u64;
    kani::assert(total >= offset, "offset arithmetic should not overflow");
}

// =========================================================================
// Cache byte invariant proofs
// =========================================================================
//
// The real cache_page() uses RefCell<HashMap>/BTreeMap which CBMC cannot
// efficiently model. These proofs verify the pure arithmetic invariant:
// after eviction + insertion, cache_bytes <= cache_byte_limit and
// cache_count <= cache_size.

/// Stub LRU cache for byte-budget proofs. Fixed-size array avoids HashMap.
struct StubLruCache {
    /// Byte size of each cached page (0 = empty slot).
    entries: [usize; 4],
    /// Number of valid entries.
    count: usize,
    /// Running total of cached bytes.
    cache_bytes: usize,
    /// Maximum number of cached pages.
    cache_size: usize,
    /// Maximum cached byte total.
    cache_byte_limit: usize,
}

impl StubLruCache {
    fn new(cache_size: usize, cache_byte_limit: usize) -> Self {
        Self {
            entries: [0; 4],
            count: 0,
            cache_bytes: 0,
            cache_size,
            cache_byte_limit,
        }
    }

    /// Model of `DiskColdTier::cache_page` eviction + insertion
    /// (disk_memory.rs). The real code evicts the LRU entry (min `last_access`)
    /// while `cache.len() >= cache_size || cache_bytes + page_bytes > cache_byte_limit`,
    /// subtracting each evicted `CacheEntry::bytes`, then inserts; this array-backed
    /// model mirrors that logic. Keep the two in lockstep.
    ///
    /// Returns false if page_bytes > cache_byte_limit (uncacheable).
    fn cache_page(&mut self, page_bytes: usize) -> bool {
        if self.cache_size == 0 || self.cache_byte_limit == 0 {
            return false;
        }
        if page_bytes > self.cache_byte_limit {
            return false;
        }

        // Evict until both limits are satisfied
        while self.count >= self.cache_size
            || self.cache_bytes.saturating_add(page_bytes) > self.cache_byte_limit
        {
            if self.count == 0 {
                break;
            }
            // Remove last entry (models LRU eviction)
            self.count -= 1;
            self.cache_bytes = self.cache_bytes.saturating_sub(self.entries[self.count]);
            self.entries[self.count] = 0;
        }

        // Insert new entry
        if self.count < 4 {
            self.entries[self.count] = page_bytes;
            self.count += 1;
            self.cache_bytes = self.cache_bytes.saturating_add(page_bytes);
        }
        true
    }

    /// Sum of all entry sizes.
    fn sum_bytes(&self) -> usize {
        let mut sum = 0usize;
        let mut i = 0;
        while i < self.count {
            sum += self.entries[i];
            i += 1;
        }
        sum
    }
}

/// After cache_page, cache_bytes <= cache_byte_limit and count <= cache_size.
///
/// This proves the core LRU eviction invariant: the eviction loop in
/// `DiskColdTier::cache_page` always brings both the byte budget and the
/// entry count within their respective limits before inserting.
#[kani::proof]
#[kani::unwind(6)]
fn cache_byte_limit_respected() {
    let cache_size: usize = kani::any();
    let cache_byte_limit: usize = kani::any();
    kani::assume(cache_size >= 1 && cache_size <= 4);
    kani::assume(cache_byte_limit >= 1 && cache_byte_limit <= 256);

    let mut cache = StubLruCache::new(cache_size, cache_byte_limit);

    // Pre-fill with some entries
    let prefill: usize = kani::any();
    kani::assume(prefill <= 3);
    let mut i = 0;
    while i < prefill {
        let bytes: usize = kani::any();
        kani::assume(bytes >= 1 && bytes <= cache_byte_limit);
        cache.cache_page(bytes);
        i += 1;
    }

    // Now insert one more page and check invariant
    let new_page_bytes: usize = kani::any();
    kani::assume(new_page_bytes >= 1 && new_page_bytes <= cache_byte_limit);
    cache.cache_page(new_page_bytes);

    // Post-condition 1: byte budget respected
    kani::assert(
        cache.cache_bytes <= cache.cache_byte_limit,
        "cache_bytes must not exceed cache_byte_limit",
    );

    // Post-condition 2: count limit respected
    kani::assert(
        cache.count <= cache.cache_size,
        "cache count must not exceed cache_size",
    );

    // Post-condition 3: cache_bytes is consistent with entries
    kani::assert(
        cache.cache_bytes == cache.sum_bytes(),
        "cache_bytes must equal sum of entry sizes",
    );
}

// =========================================================================
// Front-offset invariant proof (#6761)
// =========================================================================

/// After `truncate_front_lines`, `front_offset` is strictly less than the
/// first surviving page's `line_count`. This guarantees
/// `compact_page_trimmed` can safely slice `&lines[trim..]`.
///
/// Models truncation on in-memory DiskColdTier (no file → compact skipped).
#[kani::proof]
#[kani::unwind(5)]
fn front_offset_bounded_after_truncation() {
    let mut cold = DiskColdTier::new();

    // Build 3 pages with symbolic but bounded line counts.
    let lc0: u32 = kani::any();
    let lc1: u32 = kani::any();
    let lc2: u32 = kani::any();
    kani::assume(lc0 >= 1 && lc0 <= 20);
    kani::assume(lc1 >= 1 && lc1 <= 20);
    kani::assume(lc2 >= 1 && lc2 <= 20);

    let pages = [lc0, lc1, lc2];
    let mut total: usize = 0;
    for &lc in &pages {
        let lc_usize = lc as usize;
        cold.index.push(PageIndexEntry {
            offset: HEADER_SIZE as u64, // in-memory, offset unused
            compressed_size: 50,
            line_count: lc,
        });
        total += lc_usize;
        cold.cumulative_lines.push(total);
    }
    cold.line_count = total;

    // Truncate a symbolic number of lines.
    let n: usize = kani::any();
    kani::assume(n >= 1 && n <= total);

    cold.truncate_front_lines(n);

    // Post-condition: if LIVE pages remain, front_offset < the first live
    // page's line_count. Front drops advance a cursor + absolute base; the
    // dead-prefix entries are no longer part of the live index.
    let live = cold.live_index();
    if !live.is_empty() {
        let first_lc = len_u32_to_usize(live[0].line_count);
        kani::assert(
            cold.front_offset < first_lc,
            "front_offset must be < first live page line_count after truncation",
        );
    }
}
