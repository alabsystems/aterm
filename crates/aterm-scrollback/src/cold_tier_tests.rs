// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for in-memory cold tier — extracted from `tier_tests.rs` (#2100).

use super::*;
use crate::Line;
use crate::tier::WarmBlock;

#[test]
fn cold_tier_push_get() {
    let mut cold = ColdTier::new();

    let lines: Vec<Line> = (0..10).map(|i| Line::from(&*format!("Line {i}"))).collect();
    let warm_block = WarmBlock::from_lines(&lines);

    cold.push_block(&warm_block);

    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 1);
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
    assert_eq!(
        cold.get_line(9)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 9"
    );
    assert!(cold.get_line(10).expect("no error").is_none());
}

#[test]
fn cold_tier_multiple_pages() {
    let mut cold = ColdTier::new();

    for block_idx in 0..3 {
        let lines: Vec<Line> = (0..5)
            .map(|i| Line::from(&*format!("Block{block_idx}-Line{i}")))
            .collect();
        let warm_block = WarmBlock::from_lines(&lines);
        cold.push_block(&warm_block);
    }

    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.page_count(), 3);

    // Test access across pages
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block0-Line0"
    );
    assert_eq!(
        cold.get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line0"
    );
    assert_eq!(
        cold.get_line(10)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block2-Line0"
    );
    assert_eq!(
        cold.get_line(14)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block2-Line4"
    );
}

#[test]
fn compression_ratio() {
    // Create some realistic terminal output
    let lines: Vec<Line> = (0..100)
        .map(|i| {
            Line::from(&*format!(
                "[2024-01-01 12:00:{:02}] INFO: Processing item {} - status OK",
                i % 60,
                i
            ))
        })
        .collect();

    let uncompressed_size: usize = lines.iter().map(|l| l.len()).sum();

    let warm_block = WarmBlock::from_lines(&lines);
    let lz4_size = warm_block.compressed_size();

    let cold_page = ColdPage::from_warm_block(&warm_block).expect("no error");
    let cold_size = cold_page.compressed.len();

    // LZ4 should compress significantly
    assert!(lz4_size < uncompressed_size);
    // The cold codec should compress relative to the uncompressed input.
    assert!(cold_size < uncompressed_size);
    // With the zstd feature the cold tier re-compresses warm LZ4 blocks with
    // zstd, which achieves a better ratio than LZ4 alone. In the default
    // LZ4-only build the cold page just mirrors the warm codec.
    #[cfg(feature = "zstd")]
    assert!(cold_size < lz4_size);

    // Verify data integrity
    let decompressed = cold_page.decompress().expect("no error");
    assert_eq!(decompressed.len(), 100);
    for (i, line) in decompressed.iter().enumerate() {
        assert_eq!(
            line.to_string(),
            format!(
                "[2024-01-01 12:00:{:02}] INFO: Processing item {} - status OK",
                i % 60,
                i
            )
        );
    }
}

/// Verify push_block returns the correct line count on success.
#[test]
fn push_block_returns_line_count_on_success() {
    let mut cold = ColdTier::new();
    let lines: Vec<Line> = (0..7).map(|i| Line::from(&*format!("L{i}"))).collect();
    let block = WarmBlock::from_lines(&lines);

    let accepted = cold.push_block(&block);

    assert_eq!(accepted, 7);
    assert_eq!(cold.line_count(), 7);
}

/// Verify push_block returns 0 and does not change state when given corrupt data.
#[test]
fn push_block_returns_zero_for_corrupt_block() {
    let mut cold = ColdTier::new();

    // Add a valid page first.
    let valid_lines: Vec<Line> = (0..5).map(|i| Line::from(&*format!("V{i}"))).collect();
    let valid_block = WarmBlock::from_lines(&valid_lines);
    let accepted = cold.push_block(&valid_block);
    assert_eq!(accepted, 5);

    // Now try a corrupt block — should be silently dropped.
    let corrupt_block = WarmBlock::with_corrupt_data(10);
    let accepted = cold.push_block(&corrupt_block);

    assert_eq!(accepted, 0, "corrupt block should return 0 accepted lines");
    assert_eq!(cold.line_count(), 5, "line_count should be unchanged");
    assert_eq!(cold.page_count(), 1, "page_count should be unchanged");

    // Existing data should still be accessible.
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "V0"
    );
}

#[test]
fn truncate_back_lines_aborts_on_corrupt_boundary_page() {
    let mut cold = ColdTier::new();

    for page_idx in 0..3 {
        let lines: Vec<Line> = (0..5)
            .map(|line_idx| Line::from(&*format!("P{page_idx}-L{line_idx}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }

    let line_count_before = cold.line_count();
    let page_count_before = cold.page_count();
    let oldest_before = cold
        .get_line(0)
        .expect("oldest line read should succeed")
        .expect("oldest line should exist")
        .to_string();

    if let Some(page) = cold.pages.back_mut() {
        let old_bytes = page.compressed.len();
        page.compressed = vec![0x28, 0xB5, 0x2F, 0xFD];
        cold.bytes_used = cold.bytes_used.saturating_sub(old_bytes) + page.compressed.len();
        // Same intent as the former direct `last_page_cache = None` write: drop
        // every cached decompressed page so the next read re-decompresses the
        // corrupted bytes. The cache is now multi-slot, so go through the
        // single invalidation helper.
        cold.clear_cache();
    }
    let compressed_after_corruption = cold.compressed_size();
    assert!(
        cold.get_line(line_count_before - 1).is_err(),
        "corruption must affect the newest boundary page"
    );

    let result = cold.truncate_back_lines(3);
    assert!(
        result.is_err(),
        "truncate_back_lines should fail on corrupt boundary page"
    );

    assert_eq!(
        cold.line_count(),
        line_count_before,
        "line_count changed on decompression failure"
    );
    assert_eq!(
        cold.page_count(),
        page_count_before,
        "page_count changed on decompression failure"
    );
    assert_eq!(
        cold.compressed_size(),
        compressed_after_corruption,
        "compressed_size changed after the failed truncate attempt"
    );
    assert_eq!(
        cold.get_line(0)
            .expect("oldest line read should still succeed")
            .expect("oldest line should still exist")
            .to_string(),
        oldest_before,
        "older pages changed on decompression failure"
    );
}

/// Verify cold-tier page lookup scales logarithmically with page count.
#[test]
fn cold_tier_find_page_logarithmic_scaling() {
    fn measure_find_page_steps(page_count: usize) -> usize {
        let mut cold = ColdTier::new();

        for i in 0..page_count {
            let block = WarmBlock::from_lines(&[Line::from(&*format!("Cold-line-{i}"))]);
            cold.push_block(&block);
        }

        assert_eq!(cold.page_count(), page_count);
        assert_eq!(cold.line_count(), page_count);

        // Clear any prior counter state from earlier tests.
        let _ = take_cold_find_page_steps();

        let found = cold.find_page(page_count - 1);
        assert_eq!(found, Some(page_count - 1));

        take_cold_find_page_steps()
    }

    let small_steps = measure_find_page_steps(64);
    let large_steps = measure_find_page_steps(4096);

    assert!(
        small_steps > 0,
        "small lookup should perform binary-search steps"
    );
    assert!(
        large_steps > 0,
        "large lookup should perform binary-search steps"
    );
    assert!(
        large_steps <= small_steps.saturating_mul(4),
        "cold find_page should grow sublinearly: small={small_steps}, large={large_steps}"
    );
}

/// Verify that batch eviction via `pop_front_batch` scales linearly (#5858).
///
/// With P pages, `pop_front_batch(P)` does: O(P) drain + O(P) adjustment = O(P).
/// Compared to the old per-element `pop_front` loop which was O(P²).
/// The scaling ratio for 10x pages should be ≤ 15x (linear), not ~100x (quadratic).
#[test]
fn pop_front_cost_scales_with_page_count() {
    fn evict_all_pages_batch(page_count: usize) -> std::time::Duration {
        let mut cold = ColdTier::new();
        for i in 0..page_count {
            let block = WarmBlock::from_lines(&[Line::from(&*format!("L{i}"))]);
            cold.push_block(&block);
        }
        assert_eq!(cold.page_count(), page_count);

        let start = std::time::Instant::now();
        cold.pop_front_batch(page_count);
        start.elapsed()
    }

    // Wall-clock ratio is noisy under parallel test load (scheduler preemption,
    // especially on Windows). Take the BEST (min) of several runs at each size: the
    // minimum rejects positive scheduling noise, leaving the intrinsic O(P) cost.
    // Counts are large enough that even the small baseline sits well above timer
    // resolution, so the ratio reflects real work, not granularity.
    fn best_of(page_count: usize, runs: u32) -> std::time::Duration {
        (0..runs)
            .map(|_| evict_all_pages_batch(page_count))
            .min()
            .expect("at least one run")
    }
    let small = best_of(2_000, 5);
    let large = best_of(20_000, 5);

    let ratio = large.as_nanos() as f64 / small.as_nanos().max(1) as f64;
    eprintln!(
        "pop_front_batch scaling: 2000 pages = {small:?}, 20000 pages = {large:?}, ratio = {ratio:.1}x (linear ≈ 10x)"
    );
    // Linear O(P) scaling: 10x pages → ~10x time. Allow up to 20x for residual noise;
    // quadratic (O(P²)) would be ~100x, so this still fails loudly on a regression.
    assert!(
        ratio <= 20.0,
        "pop_front_batch scaling ratio {ratio:.1}x exceeds 20x — expected linear"
    );
}

/// Verify batch eviction preserves correctness — same result as repeated pop_front.
#[test]
fn pop_front_batch_matches_individual_pop_front() {
    // Build two identical cold tiers.
    let mut individual = ColdTier::new();
    let mut batch = ColdTier::new();
    for i in 1..=10 {
        let lines: Vec<Line> = (0..i).map(|j| Line::from(&*format!("P{i}-L{j}"))).collect();
        let block = WarmBlock::from_lines(&lines);
        individual.push_block(&block);
        batch.push_block(&block);
    }
    // Total: 1+2+...+10 = 55 lines, 10 pages.
    assert_eq!(individual.line_count(), 55);
    assert_eq!(batch.line_count(), 55);

    // Evict 5 pages individually vs batch.
    let mut individual_evicted = 0;
    for _ in 0..5 {
        individual_evicted += individual.pop_front();
    }
    let batch_evicted = batch.pop_front_batch(5);

    assert_eq!(individual_evicted, batch_evicted);
    assert_eq!(individual.line_count(), batch.line_count());
    assert_eq!(individual.page_count(), batch.page_count());

    // Verify both tiers return the same lines.
    for idx in 0..individual.line_count() {
        let a = individual.get_line(idx).expect("ok").expect("present");
        let b = batch.get_line(idx).expect("ok").expect("present");
        assert_eq!(a.to_string(), b.to_string(), "mismatch at index {idx}");
    }
}

/// Verify truncate_front_lines with partial first page (non-zero front_offset, no page drop).
#[test]
fn truncate_front_lines_partial_page() {
    let mut cold = ColdTier::new();
    let lines: Vec<Line> = (0..10).map(|i| Line::from(&*format!("L{i}"))).collect();
    let block = WarmBlock::from_lines(&lines);
    cold.push_block(&block);
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 1);

    // Remove 3 oldest lines — front_offset advances, page stays.
    cold.truncate_front_lines(3);
    assert_eq!(cold.line_count(), 7);
    assert_eq!(cold.page_count(), 1);

    // get_line(0) should return what was L3 (the 4th original line).
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "L3"
    );
    assert_eq!(
        cold.get_line(6).expect("ok").expect("present").to_string(),
        "L9"
    );
    assert!(cold.get_line(7).expect("ok").is_none());
}

/// Verify truncate_front_lines at exact page boundary drops the page.
#[test]
fn truncate_front_lines_exact_page_boundary() {
    let mut cold = ColdTier::new();
    for i in 0..3 {
        let lines: Vec<Line> = (0..5).map(|j| Line::from(&*format!("P{i}-L{j}"))).collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }
    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.page_count(), 3);

    // Remove exactly 5 lines — should drop the first page entirely.
    cold.truncate_front_lines(5);
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 2);

    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "P1-L0"
    );
    assert_eq!(
        cold.get_line(9).expect("ok").expect("present").to_string(),
        "P2-L4"
    );
    assert!(cold.get_line(10).expect("ok").is_none());
}

/// Verify truncate_front_lines crossing page boundaries (drops page + partial offset).
#[test]
fn truncate_front_lines_crosses_pages() {
    let mut cold = ColdTier::new();
    for i in 0..3 {
        let lines: Vec<Line> = (0..10)
            .map(|j| Line::from(&*format!("P{i}-L{j}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }
    assert_eq!(cold.line_count(), 30);
    assert_eq!(cold.page_count(), 3);

    // Remove 13 lines: drops first page (10), front_offset=3 on second page.
    cold.truncate_front_lines(13);
    assert_eq!(cold.line_count(), 17);
    assert_eq!(cold.page_count(), 2);

    // First available line should be P1-L3 (the 4th line of the original 2nd page).
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "P1-L3"
    );
    // Last line of original second page.
    assert_eq!(
        cold.get_line(6).expect("ok").expect("present").to_string(),
        "P1-L9"
    );
    // First line of third page.
    assert_eq!(
        cold.get_line(7).expect("ok").expect("present").to_string(),
        "P2-L0"
    );
    // Last line.
    assert_eq!(
        cold.get_line(16).expect("ok").expect("present").to_string(),
        "P2-L9"
    );
    assert!(cold.get_line(17).expect("ok").is_none());
}

/// Verify get_line correctness after interleaved truncate_front_lines + push_block.
#[test]
fn truncate_front_lines_then_push_block() {
    let mut cold = ColdTier::new();
    for i in 0..3 {
        let lines: Vec<Line> = (0..10)
            .map(|j| Line::from(&*format!("P{i}-L{j}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }
    assert_eq!(cold.line_count(), 30);

    // Truncate 13 lines (drops P0, front_offset=3 on P1).
    cold.truncate_front_lines(13);
    assert_eq!(cold.line_count(), 17);

    // Push a new page.
    let new_lines: Vec<Line> = (0..5).map(|j| Line::from(&*format!("New-L{j}"))).collect();
    cold.push_block(&WarmBlock::from_lines(&new_lines));
    assert_eq!(cold.line_count(), 22);
    assert_eq!(cold.page_count(), 3); // P1 (partial), P2, NewPage

    // Verify old content still accessible with correct offsets.
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "P1-L3"
    );
    assert_eq!(
        cold.get_line(6).expect("ok").expect("present").to_string(),
        "P1-L9"
    );
    assert_eq!(
        cold.get_line(7).expect("ok").expect("present").to_string(),
        "P2-L0"
    );
    assert_eq!(
        cold.get_line(16).expect("ok").expect("present").to_string(),
        "P2-L9"
    );
    // Verify new content accessible.
    assert_eq!(
        cold.get_line(17).expect("ok").expect("present").to_string(),
        "New-L0"
    );
    assert_eq!(
        cold.get_line(21).expect("ok").expect("present").to_string(),
        "New-L4"
    );
    assert!(cold.get_line(22).expect("ok").is_none());
}

/// Verify pop_front correctly accounts for non-zero front_offset.
#[test]
fn pop_front_with_nonzero_front_offset() {
    let mut cold = ColdTier::new();
    for i in 0..3 {
        let lines: Vec<Line> = (0..10)
            .map(|j| Line::from(&*format!("P{i}-L{j}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }

    // Set front_offset to 3 on the first page.
    cold.truncate_front_lines(3);
    assert_eq!(cold.line_count(), 27);

    // pop_front should evict only 7 logical lines (10 physical - 3 offset).
    let evicted = cold.pop_front();
    assert_eq!(evicted, 7);
    assert_eq!(cold.line_count(), 20);
    assert_eq!(cold.page_count(), 2);

    // After pop, front_offset resets. Line 0 = P1-L0.
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "P1-L0"
    );
    assert_eq!(
        cold.get_line(19).expect("ok").expect("present").to_string(),
        "P2-L9"
    );
    assert!(cold.get_line(20).expect("ok").is_none());
}

/// Verify pop_front_batch correctly accounts for non-zero front_offset.
#[test]
fn pop_front_batch_with_nonzero_front_offset() {
    let mut cold = ColdTier::new();
    for i in 0..4 {
        let lines: Vec<Line> = (0..10)
            .map(|j| Line::from(&*format!("P{i}-L{j}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }

    // Set front_offset to 3 on the first page.
    cold.truncate_front_lines(3);
    assert_eq!(cold.line_count(), 37);

    // Batch evict 2 pages: first page has 7 logical lines, second has 10.
    let evicted = cold.pop_front_batch(2);
    assert_eq!(evicted, 17); // 7 + 10
    assert_eq!(cold.line_count(), 20);
    assert_eq!(cold.page_count(), 2);

    // After batch pop, front_offset resets. Line 0 = P2-L0.
    assert_eq!(
        cold.get_line(0).expect("ok").expect("present").to_string(),
        "P2-L0"
    );
    assert_eq!(
        cold.get_line(19).expect("ok").expect("present").to_string(),
        "P3-L9"
    );
    assert!(cold.get_line(20).expect("ok").is_none());
}

/// Verify compressed_size is correct after truncate_front_lines drops pages.
#[test]
fn truncate_front_lines_updates_compressed_size() {
    let mut cold = ColdTier::new();
    for i in 0..3 {
        let lines: Vec<Line> = (0..10)
            .map(|j| Line::from(&*format!("P{i}-L{j}")))
            .collect();
        cold.push_block(&WarmBlock::from_lines(&lines));
    }
    let size_before = cold.compressed_size();
    assert!(size_before > 0);

    // Truncate 5 lines — no page dropped (partial first page).
    cold.truncate_front_lines(5);
    assert_eq!(
        cold.compressed_size(),
        size_before,
        "partial truncate should not change compressed_size"
    );
    assert_eq!(cold.compressed_size(), cold.recompute_compressed_size());

    // Truncate 5 more — first page fully consumed, should be dropped.
    cold.truncate_front_lines(5);
    assert!(cold.compressed_size() < size_before);
    assert_eq!(cold.compressed_size(), cold.recompute_compressed_size());
}

/// Verify cumulative_lines invariant is maintained through pop_front.
#[test]
fn pop_front_preserves_cumulative_invariant() {
    let mut cold = ColdTier::new();
    // Push 10 pages with varying line counts.
    for i in 1..=10 {
        let lines: Vec<Line> = (0..i).map(|j| Line::from(&*format!("P{i}-L{j}"))).collect();
        let block = WarmBlock::from_lines(&lines);
        cold.push_block(&block);
    }
    // Total: 1+2+...+10 = 55 lines, 10 pages
    assert_eq!(cold.line_count(), 55);
    assert_eq!(cold.page_count(), 10);

    // Pop 5 pages (removing lines: 1+2+3+4+5 = 15 lines)
    for _ in 0..5 {
        cold.pop_front();
    }
    assert_eq!(cold.line_count(), 40); // 55 - 15
    assert_eq!(cold.page_count(), 5);

    // Verify all remaining lines are accessible and correct.
    // Remaining pages had 6,7,8,9,10 lines.
    let mut idx = 0;
    for page_num in 6..=10 {
        for line_in_page in 0..page_num {
            let line = cold.get_line(idx).expect("no error").expect("line present");
            assert_eq!(
                line.to_string(),
                format!("P{page_num}-L{line_in_page}"),
                "mismatch at index {idx}"
            );
            idx += 1;
        }
    }
    assert_eq!(idx, 40);
}

/// ST-3 rotation oracle (in-memory twin of the disk-tier test): interleaves
/// front truncation, batch page eviction, and pushes across page boundaries,
/// checking every surviving line by content at each base state — base
/// advances, pushes over a non-zero base, amortized dead-prefix reclamation,
/// and the drop-to-empty base reset.
#[test]
fn cold_tier_rotation_interleave_matches_oracle() {
    fn push_page(
        cold: &mut ColdTier,
        oracle: &mut Vec<String>,
        next_page: &mut usize,
        lines: usize,
    ) {
        let prefix = format!("Pg{:03}", *next_page);
        *next_page += 1;
        let block_lines: Vec<Line> = (0..lines)
            .map(|i| Line::from(&*format!("{prefix}-L{i}")))
            .collect();
        let block = WarmBlock::from_lines(&block_lines);
        assert_eq!(cold.push_block(&block), lines);
        for i in 0..lines {
            oracle.push(format!("{prefix}-L{i}"));
        }
    }

    let mut cold = ColdTier::new();
    let mut oracle: Vec<String> = Vec::new();
    let mut next_page = 0usize;

    for k in 0..10 {
        push_page(&mut cold, &mut oracle, &mut next_page, 2 + (k % 4));
    }

    for k in 0..30 {
        let cut = (1 + (k % 5)).min(cold.line_count());
        cold.truncate_front_lines(cut);
        oracle.drain(..cut);
        if k % 2 == 0 {
            push_page(&mut cold, &mut oracle, &mut next_page, 1 + (k % 5));
        }
        if k % 5 == 0 {
            // Batch page eviction rides the same cursor+base maintenance.
            let evicted = cold.pop_front_batch(1.min(cold.page_count()));
            oracle.drain(..evicted);
        }
        assert_eq!(cold.line_count(), oracle.len(), "count after round {k}");
        for (i, want) in oracle.iter().enumerate() {
            let got = cold.get_line(i).unwrap().unwrap().to_string();
            assert_eq!(&got, want, "line {i} after round {k}");
        }
        assert!(cold.get_line(oracle.len()).unwrap().is_none());
    }

    // Drain to empty and refill: the base must reset with the cleared index.
    let evicted = cold.pop_front_batch(cold.page_count());
    oracle.drain(..evicted);
    let rest = cold.line_count();
    cold.truncate_front_lines(rest);
    oracle.clear();
    assert_eq!(cold.line_count(), 0);
    push_page(&mut cold, &mut oracle, &mut next_page, 4);
    for (i, want) in oracle.iter().enumerate() {
        let got = cold.get_line(i).unwrap().unwrap().to_string();
        assert_eq!(&got, want, "line {i} after refill");
    }
}
