// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for the scrollback module.
//!
//! Extracted from mod.rs (#1977).

use super::*;
use crate::line::{deserialize_lines, serialize_lines};

// =========================================================================
// Stub Scrollback for counting proofs
// =========================================================================
//
// The real Scrollback uses SmallVec/Vec/VecDeque which cause CBMC state
// explosion. This stub tracks only line counts, matching the tier promotion
// logic exactly but without storing actual Lines.
//
// This enables verification of counting invariants that would otherwise
// timeout (8+ hours) or exhaust memory (20+ GB).

/// Counter-only scrollback for Kani proofs.
///
/// Matches the tier promotion logic of the real Scrollback but uses
/// simple counters instead of collections. This avoids SmallVec/Vec
/// allocation paths that explode CBMC's state space.
struct StubScrollback {
    hot_count: usize,
    warm_count: usize,
    cold_count: usize,
    hot_limit: usize,
    warm_limit: usize,
    block_size: usize,
    line_count: usize,
    line_limit: Option<usize>,
}

impl StubScrollback {
    /// Create with the same signature as real Scrollback::with_block_size.
    fn with_block_size(
        hot_limit: usize,
        warm_limit: usize,
        _memory_budget: usize,
        block_size: usize,
    ) -> Self {
        let hot_limit = hot_limit.max(1);
        let block_size = block_size.max(1).min(hot_limit);
        Self {
            hot_count: 0,
            warm_count: 0,
            cold_count: 0,
            hot_limit,
            warm_limit,
            block_size,
            line_count: 0,
            line_limit: None,
        }
    }

    /// Push a line assuming cold-tier eviction succeeds.
    ///
    /// Most proofs don't care about the failure path, so they use the
    /// production success case by default.
    fn push_line(&mut self) {
        self.push_line_with_cold_acceptance(true);
    }

    /// Push a line while choosing whether warm→cold eviction succeeds.
    ///
    /// This matches the real `Scrollback::push_line` control flow:
    /// 1. promote hot→warm when hot is full
    /// 2. evict warm→cold if warm exceeds its limit
    /// 3. if cold rejects the block, drop those lines from `line_count`
    /// 4. append the new hot line
    /// 5. enforce line_limit via `truncate`
    fn push_line_with_cold_acceptance(&mut self, cold_accepts: bool) {
        // Promote if hot tier is full (matches real logic)
        if self.hot_count >= self.hot_limit {
            self.promote_hot_to_warm(cold_accepts);
        }
        self.hot_count += 1;
        self.line_count += 1;

        if let Some(limit) = self.line_limit {
            if self.line_count > limit {
                self.truncate(limit);
            }
        }
    }

    /// Promote hot->warm (matches real logic in promote_hot_to_warm).
    fn promote_hot_to_warm(&mut self, cold_accepts: bool) {
        if self.hot_count < self.block_size {
            return;
        }
        // Take block_size lines from hot to warm
        self.hot_count -= self.block_size;
        self.warm_count += self.block_size;

        // Evict warm->cold if over limit (matches real logic)
        if self.warm_count > self.warm_limit {
            self.evict_warm_to_cold(cold_accepts);
        }
    }

    /// Evict warm->cold (matches real logic in `evict_warm_to_cold`).
    fn evict_warm_to_cold(&mut self, cold_accepts: bool) {
        // Real implementation pops one WarmBlock at a time
        // WarmBlock contains block_size lines
        if self.warm_count >= self.block_size {
            self.warm_count -= self.block_size;
            if cold_accepts {
                self.cold_count += self.block_size;
            } else {
                self.line_count = self.line_count.saturating_sub(self.block_size);
            }
        }
    }

    fn line_count(&self) -> usize {
        self.line_count
    }

    fn hot_line_count(&self) -> usize {
        self.hot_count
    }

    fn warm_line_count(&self) -> usize {
        self.warm_count
    }

    fn cold_line_count(&self) -> usize {
        self.cold_count
    }

    fn clear(&mut self) {
        self.hot_count = 0;
        self.warm_count = 0;
        self.cold_count = 0;
        self.line_count = 0;
    }

    fn line_limit(&self) -> Option<usize> {
        self.line_limit
    }

    fn set_line_limit(&mut self, limit: Option<usize>) {
        self.line_limit = limit;
        if let Some(limit) = limit {
            if self.line_count > limit {
                self.truncate(limit);
            }
        }
    }

    /// Count-level model of `Scrollback::truncate`.
    ///
    /// Matches the real implementation's two paths:
    /// - Fast path (n <= hot_count): clear warm/cold, keep last n in hot.
    ///   No promotion loop — hot was already bounded by push invariant.
    /// - Slow path (n > hot_count): rebuild all kept lines into hot, then
    ///   promote excess to restore tier structure (#5862).
    fn truncate(&mut self, n: usize) {
        if n == 0 {
            self.clear();
            return;
        }
        if n >= self.line_count {
            return;
        }

        if n <= self.hot_count {
            // Fast path: all kept lines are in hot tier (real code line 218-223)
            self.cold_count = 0;
            self.warm_count = 0;
            self.hot_count = n;
            self.line_count = n;
        } else {
            // Slow path: lines span multiple tiers — rebuild into hot
            self.hot_count = n;
            self.warm_count = 0;
            self.cold_count = 0;
            self.line_count = n;

            // Restore tier structure (real code line 251-253)
            while self.hot_count >= self.hot_limit {
                self.promote_hot_to_warm(true);
            }
        }
    }

    /// Count-level model of `Scrollback::remove_newest`.
    ///
    /// Matches the real implementation's two paths:
    /// - Fast path (n <= hot_count): truncate back of hot.
    /// - Slow path: rebuild oldest lines into hot, then promote excess
    ///   to restore tier structure (#5862).
    fn remove_newest(&mut self, n: usize) {
        if n == 0 || self.line_count == 0 {
            return;
        }
        if n >= self.line_count {
            self.clear();
            return;
        }

        let keep = self.line_count - n;
        if n <= self.hot_count {
            // Fast path: remove from hot only
            self.hot_count -= n;
            self.line_count -= n;
        } else {
            // Slow path: rebuild oldest `keep` lines into hot
            self.hot_count = keep;
            self.warm_count = 0;
            self.cold_count = 0;
            self.line_count = keep;

            // Restore tier structure
            while self.hot_count >= self.hot_limit {
                self.promote_hot_to_warm(true);
            }
        }
    }

    fn invariant_holds(&self) -> bool {
        self.line_count == self.hot_count + self.warm_count + self.cold_count
    }
}

// =========================================================================
// Proofs using stub (counting invariants - no Line content needed)
// =========================================================================

/// Line count is always accurate (stub version).
///
/// Verifies: line_count == hot + warm + cold after any number of pushes.
#[kani::proof]
#[kani::unwind(11)]
fn line_count_accurate() {
    let mut sb = StubScrollback::with_block_size(5, 10, 10_000_000, 2);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 10);

    for _ in 0..push_count {
        sb.push_line();
    }

    kani::assert(sb.line_count() == push_count, "line count mismatch");

    // Verify total equals sum of tiers
    let total = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    kani::assert(sb.line_count() == total, "tier sum mismatch");
}

/// Hot tier never exceeds limit (stub version).
///
/// Uses fixed representative values covering boundary cases.
#[kani::proof]
#[kani::unwind(13)]
fn hot_bounded() {
    const TEST_CASES: [(usize, usize); 4] = [
        (5, 2),  // Under limit
        (5, 5),  // At limit
        (5, 12), // Over limit (triggers tier transition)
        (3, 10), // Small limit, many pushes
    ];

    let case_idx: usize = kani::any();
    kani::assume(case_idx < TEST_CASES.len());
    let (hot_limit, push_count) = TEST_CASES[case_idx];

    let mut sb = StubScrollback::with_block_size(hot_limit, 100, 10_000_000, 2);

    for _ in 0..push_count {
        sb.push_line();
    }

    kani::assert(sb.hot_line_count() <= hot_limit, "hot tier exceeded limit");
}

/// Tier transitions preserve line count invariant (stub version).
///
/// Verifies counting is correct through hot->warm and warm->cold transitions.
#[kani::proof]
#[kani::unwind(22)]
fn tier_transition_preserves_count() {
    const TEST_CASES: [(usize, usize, usize); 4] = [
        (5, 10, 3),  // Under hot limit - no transitions
        (5, 10, 8),  // Over hot, under warm - hot->warm only
        (5, 10, 18), // Over both - hot->warm->cold
        (3, 5, 21),  // Small limits, many pushes - multiple transitions
    ];

    let case_idx: usize = kani::any();
    kani::assume(case_idx < TEST_CASES.len());
    let (hot_limit, warm_limit, push_count) = TEST_CASES[case_idx];

    let mut sb = StubScrollback::with_block_size(hot_limit, warm_limit, 10_000_000, 2);

    for _ in 0..push_count {
        sb.push_line();
    }

    // Verify invariant: total equals sum of tiers
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    let cold = sb.cold_line_count();
    let total = sb.line_count();

    kani::assert(
        total == hot + warm + cold,
        "line count doesn't match tier sum after transitions",
    );
    kani::assert(total == push_count, "line count doesn't match push count");
}

/// Clear resets all tiers to empty (stub version).
#[kani::proof]
#[kani::unwind(11)]
fn clear_resets_all() {
    let mut sb = StubScrollback::with_block_size(5, 10, 10_000_000, 2);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 10);

    for _ in 0..push_count {
        sb.push_line();
    }

    sb.clear();

    kani::assert(sb.line_count() == 0, "line count not zero after clear");
    kani::assert(sb.hot_line_count() == 0, "hot tier not empty after clear");
    kani::assert(sb.warm_line_count() == 0, "warm tier not empty after clear");
    kani::assert(sb.cold_line_count() == 0, "cold tier not empty after clear");
}

/// Setting a line limit enforces truncation and stores the configured value.
///
/// Verifies both setter/getter behavior and the primary invariant:
/// `line_count <= line_limit` once a limit is applied.
#[kani::proof]
#[kani::unwind(22)]
fn line_limit_enforced() {
    let mut sb = StubScrollback::with_block_size(5, 10, 10_000_000, 2);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 10);
    for _ in 0..push_count {
        sb.push_line();
    }

    let limit: usize = kani::any();
    kani::assume(limit <= 10);
    sb.set_line_limit(Some(limit));

    kani::assert(
        sb.line_limit() == Some(limit),
        "line limit setter/getter mismatch",
    );
    kani::assert(
        sb.line_count() <= limit,
        "line count exceeded configured line limit",
    );
    kani::assert(
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count() == sb.line_count(),
        "tier sum must remain consistent after line-limit truncation",
    );

    // Clearing the limit should preserve the current count and expose `None`.
    let preserved_count = sb.line_count();
    sb.set_line_limit(None);
    kani::assert(sb.line_limit().is_none(), "line limit should be cleared");
    kani::assert(
        sb.line_count() == preserved_count,
        "clearing line limit must not change line count",
    );
    kani::assert(
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count() == sb.line_count(),
        "tier sum must remain consistent after clearing line limit",
    );
}

/// Warm→cold rejection must not desync `line_count` from the tier totals.
///
/// Models the #5817 regression path with the same representative limits as the
/// unit test: 5 hot lines, 10 warm lines, 5-line blocks. Each push may choose
/// whether a warm→cold eviction succeeds; when it fails, the dropped block must
/// be removed from `line_count`.
#[kani::proof]
#[kani::unwind(21)]
fn cold_push_drop_preserves_line_count_invariant() {
    let mut sb = StubScrollback::with_block_size(5, 10, 10_000_000, 5);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 20);

    let mut i = 0;
    while i < push_count {
        let cold_accepts: bool = kani::any();
        sb.push_line_with_cold_acceptance(cold_accepts);

        kani::assert(
            sb.invariant_holds(),
            "line_count must equal hot + warm + cold after push/evict",
        );
        i += 1;
    }
}

/// Truncate preserves the tier-sum invariant even after dropped warm blocks.
#[kani::proof]
#[kani::unwind(21)]
fn truncate_after_cold_push_drop_preserves_line_count_invariant() {
    let mut sb = StubScrollback::with_block_size(5, 10, 10_000_000, 5);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 20);

    let mut i = 0;
    while i < push_count {
        let cold_accepts: bool = kani::any();
        sb.push_line_with_cold_acceptance(cold_accepts);
        i += 1;
    }

    let pre_line_count = sb.line_count();
    let keep: usize = kani::any();
    kani::assume(keep <= 20);
    sb.truncate(keep);

    kani::assert(
        sb.invariant_holds(),
        "truncate must preserve line_count == hot + warm + cold",
    );
    kani::assert(
        sb.line_count() == pre_line_count.min(keep),
        "truncate must keep exactly min(pre_line_count, keep) lines",
    );
}

/// Hot tier bounded after truncate rebuild (#5862).
///
/// Verifies `hot_count <= hot_limit` after truncate for both paths:
/// - Fast path (keep <= hot_count): hot bounded by push invariant.
/// - Slow path (keep > hot_count): promotion loop restores `hot < hot_limit`.
/// Also verifies tier-sum invariant and exact line count preservation.
#[kani::proof]
#[kani::unwind(22)]
fn hot_bounded_after_truncate_rebuild() {
    const TEST_CASES: [(usize, usize, usize); 4] = [
        (5, 10, 20), // Many lines, typical limits
        (3, 6, 18),  // Small hot_limit, many pushes
        (5, 10, 8),  // Moderate — just past hot_limit
        (4, 8, 16),  // Even block boundaries
    ];

    let case_idx: usize = kani::any();
    kani::assume(case_idx < TEST_CASES.len());
    let (hot_limit, warm_limit, push_count) = TEST_CASES[case_idx];

    let mut sb = StubScrollback::with_block_size(hot_limit, warm_limit, 10_000_000, 2);
    for _ in 0..push_count {
        sb.push_line();
    }

    // Nondeterministic keep value exercises both fast and slow paths
    let keep: usize = kani::any();
    kani::assume(keep > 0 && keep < push_count);
    sb.truncate(keep);

    // After any truncate that actually modifies state:
    // - Fast path gives hot_count <= hot_limit (bounded by push invariant)
    // - Slow path gives hot_count < hot_limit (from promotion loop)
    // Both satisfy the weaker bound.
    kani::assert(
        sb.hot_line_count() <= sb.hot_limit,
        "hot tier must be <= hot_limit after truncate",
    );
    kani::assert(
        sb.invariant_holds(),
        "tier sum invariant must hold after truncate",
    );
    kani::assert(
        sb.line_count() == keep,
        "truncate must keep exactly requested lines",
    );
}

/// Hot tier bounded after remove_newest slow-path rebuild (#5862).
///
/// When remove_newest removes more lines than exist in hot, the slow path
/// rebuilds the oldest lines into hot then promotes excess. This proof
/// verifies `hot_count <= hot_limit` for both fast and slow paths.
#[kani::proof]
#[kani::unwind(22)]
fn hot_bounded_after_remove_newest_rebuild() {
    const TEST_CASES: [(usize, usize, usize); 4] = [
        (5, 10, 20), // Many lines, typical limits
        (3, 6, 18),  // Small hot_limit, many pushes
        (5, 10, 8),  // Moderate
        (4, 8, 16),  // Even block boundaries
    ];

    let case_idx: usize = kani::any();
    kani::assume(case_idx < TEST_CASES.len());
    let (hot_limit, warm_limit, push_count) = TEST_CASES[case_idx];

    let mut sb = StubScrollback::with_block_size(hot_limit, warm_limit, 10_000_000, 2);
    for _ in 0..push_count {
        sb.push_line();
    }

    let remove: usize = kani::any();
    kani::assume(remove > 0 && remove < push_count);
    sb.remove_newest(remove);

    let expected_count = push_count - remove;
    kani::assert(
        sb.hot_line_count() <= sb.hot_limit,
        "hot tier must be <= hot_limit after remove_newest",
    );
    kani::assert(
        sb.invariant_holds(),
        "tier sum invariant must hold after remove_newest",
    );
    kani::assert(
        sb.line_count() == expected_count,
        "remove_newest must leave exactly push_count - remove lines",
    );
}

// =========================================================================
// ColdTier pop_front eviction proofs (#5476)
// =========================================================================
//
// W14 added ColdTier::pop_front() for FIFO eviction under memory pressure.
// The real ColdTier depends on Zstd/LZ4 compression which causes CBMC state
// explosion. This stub models the counting + cumulative-index invariants.

/// Stub ColdTier for counting invariant proofs.
///
/// Models page-level line counts and the cumulative index array without
/// compression. Fixed-size arrays (max 4 pages) keep CBMC tractable.
struct StubColdTier {
    page_line_counts: [usize; 4],
    page_count: usize,
    line_count: usize,
    cumulative_lines: [usize; 4],
}

impl StubColdTier {
    fn new() -> Self {
        Self {
            page_line_counts: [0; 4],
            page_count: 0,
            line_count: 0,
            cumulative_lines: [0; 4],
        }
    }

    /// Push a page with given line count (models push_block success path).
    fn push_page(&mut self, lines: usize) {
        if self.page_count >= 4 {
            return;
        }
        let cumulative = if self.page_count == 0 {
            lines
        } else {
            self.cumulative_lines[self.page_count - 1] + lines
        };
        self.page_line_counts[self.page_count] = lines;
        self.cumulative_lines[self.page_count] = cumulative;
        self.page_count += 1;
        self.line_count += lines;
    }

    /// Pop front page (models real ColdTier::pop_front exactly).
    ///
    /// Real code: pages.pop_front(), line_count -= evicted,
    /// cumulative_lines.remove(0), then subtract evicted from all remaining.
    fn pop_front(&mut self) -> usize {
        if self.page_count == 0 {
            return 0;
        }
        let evicted = self.page_line_counts[0];
        self.line_count -= evicted;

        // Shift arrays left (models VecDeque pop_front + Vec remove(0))
        // and subtract evicted from cumulative (models the for-loop adjustment).
        for i in 0..self.page_count - 1 {
            self.page_line_counts[i] = self.page_line_counts[i + 1];
            self.cumulative_lines[i] = self.cumulative_lines[i + 1] - evicted;
        }
        self.page_count -= 1;

        evicted
    }

    /// line_count == sum(page_line_counts[0..page_count])
    fn line_count_consistent(&self) -> bool {
        let mut sum = 0usize;
        let mut i = 0;
        while i < self.page_count {
            sum += self.page_line_counts[i];
            i += 1;
        }
        self.line_count == sum
    }

    /// cumulative_lines[i] == sum(page_line_counts[0..=i]) for all i
    fn cumulative_consistent(&self) -> bool {
        let mut sum = 0usize;
        let mut i = 0;
        while i < self.page_count {
            sum += self.page_line_counts[i];
            if self.cumulative_lines[i] != sum {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// pop_front on empty cold tier returns 0 and is a no-op.
#[kani::proof]
fn cold_pop_front_empty_returns_zero() {
    let mut cold = StubColdTier::new();
    let evicted = cold.pop_front();
    kani::assert(evicted == 0, "pop_front on empty must return 0");
    kani::assert(cold.line_count == 0, "line_count must stay 0");
    kani::assert(cold.page_count == 0, "page_count must stay 0");
}

/// pop_front preserves line_count == sum(page_line_counts) and
/// cumulative_lines[i] == sum(page_line_counts[0..=i]).
#[kani::proof]
#[kani::unwind(5)]
fn cold_pop_front_preserves_invariants() {
    let mut cold = StubColdTier::new();

    let n_pages: usize = kani::any();
    kani::assume(n_pages >= 1 && n_pages <= 4);

    let mut i = 0;
    while i < n_pages {
        let lines: usize = kani::any();
        kani::assume(lines >= 1 && lines <= 10);
        cold.push_page(lines);
        i += 1;
    }

    // Invariants hold before pop
    kani::assert(cold.line_count_consistent(), "pre: line_count invariant");
    kani::assert(cold.cumulative_consistent(), "pre: cumulative invariant");

    let pre_line_count = cold.line_count;
    let evicted = cold.pop_front();

    kani::assert(evicted >= 1, "evicted at least 1 line");
    kani::assert(
        cold.line_count == pre_line_count - evicted,
        "line_count decreased by evicted",
    );
    kani::assert(cold.page_count == n_pages - 1, "page_count decreased by 1");

    // Invariants hold after pop
    kani::assert(cold.line_count_consistent(), "post: line_count invariant");
    kani::assert(cold.cumulative_consistent(), "post: cumulative invariant");
}

/// Memory pressure eviction loop: scrollback line_count == hot+warm+cold
/// after popping 1..n cold pages (models handle_memory_pressure).
#[kani::proof]
#[kani::unwind(5)]
fn cold_eviction_loop_preserves_scrollback_total() {
    let mut cold = StubColdTier::new();

    let n_pages: usize = kani::any();
    kani::assume(n_pages >= 1 && n_pages <= 3);

    let mut i = 0;
    while i < n_pages {
        let lines: usize = kani::any();
        kani::assume(lines >= 1 && lines <= 5);
        cold.push_page(lines);
        i += 1;
    }

    // Model scrollback: line_count = hot_warm + cold
    let hot_warm_count: usize = kani::any();
    kani::assume(hot_warm_count <= 10);
    let mut scrollback_line_count = hot_warm_count + cold.line_count;

    // Evict some cold pages (models the while loop in handle_memory_pressure)
    let evict_count: usize = kani::any();
    kani::assume(evict_count >= 1 && evict_count <= n_pages);

    let mut j = 0;
    while j < evict_count {
        let evicted = cold.pop_front();
        scrollback_line_count -= evicted;
        j += 1;
    }

    kani::assert(
        scrollback_line_count == hot_warm_count + cold.line_count,
        "scrollback line_count == hot+warm + cold after eviction",
    );
    kani::assert(
        cold.line_count_consistent(),
        "cold invariant after eviction loop",
    );
}

// =========================================================================
// Proofs using real Scrollback (need actual Line behavior)
// =========================================================================
//
// These proofs verify get_line() behavior and need the real implementation.
// They use small bounds and work within CBMC's limits.

/// Get line returns valid line for valid index.
///
/// Uses minimal bounds (push 1-2 lines) because real Scrollback with
/// Line::default() causes CBMC state explosion from SmallVec/Vec allocations.
#[kani::proof]
#[kani::unwind(3)]
fn get_line_valid() {
    let mut sb = Scrollback::with_block_size(3, 5, 10_000_000, 2);

    let push_count: usize = kani::any();
    kani::assume(push_count >= 1 && push_count <= 2);

    for _ in 0..push_count {
        sb.push_line(Line::default());
    }

    let idx: usize = kani::any();
    kani::assume(idx < push_count);

    let result = sb.get_line(idx);
    kani::assert(
        matches!(result, Ok(Some(_))),
        "valid index must return Ok(Some(_))",
    );
}

/// Get line returns None for out-of-bounds index.
///
/// Uses minimal bounds (push 0-2 lines) because real Scrollback with
/// Line::default() causes CBMC state explosion from SmallVec/Vec allocations.
#[kani::proof]
#[kani::unwind(3)]
fn get_line_out_of_bounds() {
    let mut sb = Scrollback::with_block_size(3, 5, 10_000_000, 2);

    let push_count: usize = kani::any();
    kani::assume(push_count <= 2);

    for _ in 0..push_count {
        sb.push_line(Line::default());
    }

    let idx: usize = kani::any();
    kani::assume(idx >= push_count && idx < 10);

    let result = sb.get_line(idx);
    kani::assert(
        matches!(result, Ok(None)),
        "out-of-bounds index must return Ok(None)",
    );
}

// =========================================================================
// Pressure watermark boundary proofs (#5233)
// =========================================================================
//
// The scrollback watermark drives PTY intake throttling. These proofs verify
// the boundary conditions of the threshold computation (threshold_bytes) and
// the stateless upward-transition logic of update_watermark_level.
//
// The current API uses WatermarkLevel with hysteresis (stateful transitions).
// For Kani proofs we verify the stateless upward path: given a fresh Green
// level, the immediate transition based on budgeted_bytes vs thresholds.
// This covers the same invariants as the original pressure_level_from_budget
// proofs since upward transitions are immediate (only downward use hysteresis).

/// Stateless watermark level computation for Kani proofs.
/// Matches the upward-transition logic of `update_watermark_level` starting
/// from Green (the initial/default state).
fn watermark_level_stateless(
    budgeted_bytes: usize,
    yellow_threshold: usize,
    red_threshold: usize,
) -> WatermarkLevel {
    if budgeted_bytes >= red_threshold {
        WatermarkLevel::Red
    } else if budgeted_bytes >= yellow_threshold {
        WatermarkLevel::Yellow
    } else {
        WatermarkLevel::Green
    }
}

/// Yellow threshold is always in [0, memory_budget].
/// When budget is zero, yellow threshold is also zero.
#[kani::proof]
fn yellow_threshold_bounded() {
    let memory_budget: usize = kani::any();
    kani::assume(memory_budget <= 100_000);

    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget);

    if memory_budget == 0 {
        kani::assert(yellow == 0, "zero budget → zero threshold");
    } else {
        kani::assert(
            yellow <= memory_budget,
            "yellow threshold must be <= memory_budget",
        );
    }
}

/// Watermark level is monotonically non-decreasing in budgeted_bytes.
///
/// For fixed thresholds, increasing budgeted_bytes must produce
/// an equal or higher watermark level. This ensures the PTY throttle
/// never relaxes as memory usage grows.
#[kani::proof]
fn pressure_monotonic_in_budgeted_bytes() {
    let memory_budget: usize = kani::any();
    kani::assume(memory_budget <= 10_000);

    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget);
    let red = threshold_bytes(DEFAULT_RED_PERCENT, memory_budget);

    let budgeted_a: usize = kani::any();
    let budgeted_b: usize = kani::any();
    kani::assume(budgeted_a <= budgeted_b);
    kani::assume(budgeted_b <= 20_000);

    let level_a = watermark_level_stateless(budgeted_a, yellow, red);
    let level_b = watermark_level_stateless(budgeted_b, yellow, red);

    kani::assert(
        level_a <= level_b,
        "watermark must be monotonically non-decreasing in budgeted_bytes",
    );
}

/// With zero budget, all thresholds are zero, so any usage (including zero)
/// compares as `>= 0`, hitting Red. In production, zero-budget scrollbacks
/// never call update_watermark_level (no push_line activity), so the
/// watermark stays at the default Green. This proof verifies that
/// threshold_bytes produces 0 for 0 budget (the threshold computation
/// invariant), and that a positive budget always produces a positive
/// red threshold (meaning Red is unreachable at zero usage).
#[kani::proof]
fn zero_budget_thresholds_are_zero() {
    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, 0);
    let red = threshold_bytes(DEFAULT_RED_PERCENT, 0);
    kani::assert(yellow == 0, "yellow threshold for zero budget must be 0");
    kani::assert(red == 0, "red threshold for zero budget must be 0");
}

/// Budget >= 2 always produces a positive red threshold.
/// (Budget = 1 → threshold_bytes(95, 1) = 0 due to integer division.)
/// This means budgeted_bytes = 0 is always below red,
/// ensuring freshly-created scrollbacks start in a non-Red state.
#[kani::proof]
fn positive_budget_red_threshold_positive() {
    let memory_budget: usize = kani::any();
    kani::assume(memory_budget >= 2 && memory_budget <= 100_000);

    let red = threshold_bytes(DEFAULT_RED_PERCENT, memory_budget);
    kani::assert(red >= 1, "budget >= 2 must produce positive red threshold");

    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget);
    // Zero usage with positive thresholds → Green
    let level = watermark_level_stateless(0, yellow, red);
    kani::assert(
        level == WatermarkLevel::Green,
        "zero usage with positive thresholds must be Green",
    );
}

/// At exactly the yellow threshold, level is Yellow; one byte below is Green.
///
/// Requires memory_budget >= 2 so that yellow_threshold < red_threshold.
#[kani::proof]
fn exact_yellow_boundary() {
    let memory_budget: usize = kani::any();
    kani::assume(memory_budget >= 2 && memory_budget <= 100_000);

    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget);
    let red = threshold_bytes(DEFAULT_RED_PERCENT, memory_budget);
    kani::assume(yellow < red); // Ensures Yellow band exists

    let level = watermark_level_stateless(yellow, yellow, red);
    kani::assert(
        level == WatermarkLevel::Yellow,
        "exactly at yellow threshold must be Yellow",
    );

    // One byte below must be Green
    if yellow > 0 {
        let level_below = watermark_level_stateless(yellow - 1, yellow, red);
        kani::assert(
            level_below == WatermarkLevel::Green,
            "one byte below yellow threshold must be Green",
        );
    }
}

/// At exactly red threshold, level is Red. Above is also Red.
/// One byte below red threshold is not Red (Yellow or Green).
#[kani::proof]
fn exact_red_boundary() {
    let memory_budget: usize = kani::any();
    kani::assume(memory_budget >= 2 && memory_budget <= 100_000);

    let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget);
    let red = threshold_bytes(DEFAULT_RED_PERCENT, memory_budget);
    kani::assume(red > 0); // Ensures Red boundary is reachable

    let level_at = watermark_level_stateless(red, yellow, red);
    kani::assert(
        level_at == WatermarkLevel::Red,
        "at red threshold must be Red",
    );

    // Above red threshold is also Red
    let above: usize = kani::any();
    kani::assume(above > red && above <= red + 1000);
    let level_above = watermark_level_stateless(above, yellow, red);
    kani::assert(
        level_above == WatermarkLevel::Red,
        "above red threshold must be Red",
    );

    // One byte below red threshold must not be Red
    let level_below = watermark_level_stateless(red - 1, yellow, red);
    kani::assert(
        level_below != WatermarkLevel::Red,
        "one byte below red threshold must not be Red",
    );
}

// =========================================================================
// Page serialization round-trip proofs (#2603)
// =========================================================================
//
// DiskBacked::push_str + DiskBacked::get_line exercises the full
// serialization pipeline: Line → serialize → compress → disk → decompress
// → deserialize → Line. Kani cannot model the DiskBacked type directly
// because it requires:
// - File I/O (DiskColdTier::with_config creates/opens files)
// - Zstd compression (FFI to C library, opaque to CBMC)
// - LZ4 compression in warm tier
// - mmap operations (OS-level page mapping)
// - RefCell<HashMap/BTreeMap> (state explosion from collection internals)
//
// Instead, we verify the serialization layer that DiskBacked depends on:
// CellAttrs, Line, and block-level serialize/deserialize round-trips.
// A property test in disk_backed_tests.rs covers the full DiskBacked
// push_str + get_line round-trip with real disk I/O.

/// CellAttrs serialize/deserialize round-trip.
///
/// Verifies that arbitrary CellAttrs survive serialization without data loss.
/// This is the innermost serialization primitive used by warm and cold tiers.
#[kani::proof]
fn cell_attrs_serialize_roundtrip() {
    let fg: u32 = kani::any();
    let bg: u32 = kani::any();
    let flags: u16 = kani::any();

    let attrs = CellAttrs { fg, bg, flags };
    let bytes = attrs.serialize();
    let recovered = CellAttrs::deserialize(&bytes);

    kani::assert(
        recovered.is_some(),
        "deserialize must succeed for valid serialized data",
    );
    let recovered = recovered.unwrap();
    kani::assert(recovered.fg == fg, "fg must round-trip");
    kani::assert(recovered.bg == bg, "bg must round-trip");
    kani::assert(recovered.flags == flags, "flags must round-trip");
}

/// CellAttrs deserialize rejects truncated input.
///
/// Any slice shorter than 10 bytes must return None.
#[kani::proof]
fn cell_attrs_deserialize_rejects_short() {
    let len: usize = kani::any();
    kani::assume(len < 10);

    // Use a fixed buffer; only `len` bytes are "valid"
    let buf = [0u8; 9];
    let result = CellAttrs::deserialize(&buf[..len]);
    kani::assert(result.is_none(), "short input must return None");
}

/// Line serialize/deserialize round-trip for plain text.
///
/// Verifies content integrity through the serialization pipeline for lines
/// without attributes or hyperlinks. Uses a small fixed content to keep
/// CBMC tractable (SmallVec + Vec cause state explosion with symbolic sizes).
#[kani::proof]
#[kani::unwind(20)]
fn line_serialize_roundtrip_plain() {
    // Use small fixed content strings to avoid SmallVec state explosion.
    // The serialization format is identical regardless of content length.
    let case: u8 = kani::any();
    kani::assume(case < 4);

    let content: &[u8] = match case {
        0 => b"",
        1 => b"A",
        2 => b"Hi",
        3 => b"abc",
        _ => unreachable!(),
    };

    let line = Line::from(std::str::from_utf8(content).unwrap());
    let serialized = line.serialize();
    let recovered = Line::deserialize(&serialized);

    kani::assert(
        recovered.is_some(),
        "deserialize must succeed for valid serialized line",
    );
    let recovered = recovered.unwrap();
    kani::assert(
        recovered.as_bytes() == content,
        "line content must round-trip exactly",
    );
    kani::assert(recovered.attrs().is_none(), "plain line has no attrs");
    kani::assert(
        recovered.hyperlinks().is_none(),
        "plain line has no hyperlinks",
    );
}

/// Block-level serialize_lines/deserialize_lines round-trip.
///
/// Verifies that a small block of lines survives block serialization,
/// which is the format used by warm→cold tier promotion. Uses 1-3
/// fixed-content lines to stay within CBMC's state space.
#[kani::proof]
#[kani::unwind(6)]
fn block_serialize_roundtrip() {
    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    // Build a fixed set of lines (symbolic count, fixed content per slot)
    let all_lines = [
        Line::from("first"),
        Line::from("second"),
        Line::from("third"),
    ];

    let lines: Vec<Line> = all_lines[..count].to_vec();
    let block = serialize_lines(&lines);
    let recovered = deserialize_lines(&block);

    kani::assert(
        recovered.len() == count,
        "block round-trip must preserve line count",
    );

    // Verify content matches for each line
    let mut i = 0;
    while i < count {
        kani::assert(
            recovered[i].as_bytes() == lines[i].as_bytes(),
            "block round-trip must preserve line content",
        );
        i += 1;
    }
}
