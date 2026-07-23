// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for tiered storage (hot/warm/cold) — extracted from `tier.rs` (#2100).

use super::*;
use crate::HotTier;
use crate::Scrollback;
use std::cell::Cell;

impl WarmBlock {
    /// Create a WarmBlock with corrupt compressed data (invalid LZ4).
    ///
    /// The block reports `line_count` lines but its compressed data cannot be
    /// decompressed. Used to test error paths in cold tier push and warm-to-cold
    /// eviction where re-compression fails.
    pub(crate) fn with_corrupt_data(line_count: usize) -> Self {
        Self {
            // Invalid LZ4: valid 4-byte size prefix (small) + garbage payload
            compressed: vec![0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF],
            line_count,
            decompress_failures: Cell::new(0),
        }
    }
}

impl WarmTier {
    pub(crate) fn oldest_block_bytes(&self) -> Option<Vec<u8>> {
        self.blocks.front().map(|block| block.compressed.clone())
    }

    pub(crate) fn restore_oldest_block(&mut self, compressed: Vec<u8>) {
        if let Some(block) = self.blocks.front_mut() {
            let old_bytes = block.compressed.len();
            let new_bytes = compressed.len();
            block.compressed = compressed;
            self.bytes_used.set(WarmTier::adjust_bytes(
                self.bytes_used.get(),
                old_bytes,
                new_bytes,
            ));
            self.budgeted_bytes = WarmTier::adjust_bytes(self.budgeted_bytes, old_bytes, new_bytes);
            self.clear_cache();
        }
    }

    /// Mutable access to the underlying block deque for test corruption. (#5947)
    pub(crate) fn blocks_mut(&mut self) -> &mut std::collections::VecDeque<WarmBlock> {
        &mut self.blocks
    }

    /// Insert a corrupted warm block at the front (oldest position).
    ///
    /// Used by quarantine tests (#5947) to simulate a corrupt block that
    /// fails eviction to cold tier.
    pub(crate) fn push_front_corrupt(&mut self, line_count: usize) {
        let block = WarmBlock::with_corrupt_data(line_count);
        self.push_front(block);
    }
}

// Hot tier tests
#[test]
fn hot_tier_push_get() {
    let mut hot = HotTier::new();
    hot.push(Line::from("Line 0"));
    hot.push(Line::from("Line 1"));
    hot.push(Line::from("Line 2"));

    assert_eq!(hot.len(), 3);
    assert_eq!(hot.get(0).unwrap().to_string(), "Line 0");
    assert_eq!(hot.get(1).unwrap().to_string(), "Line 1");
    assert_eq!(hot.get(2).unwrap().to_string(), "Line 2");
    assert!(hot.get(3).is_none());
}

/// A configured block_size above the disk-decode line-count cap must be clamped,
/// so a promoted warm/cold/disk block never holds more lines than the decoder will
/// reconstruct (else a legitimate oversized block decodes short and fails its
/// count validation — codex round-4 finding 2).
#[test]
fn block_size_clamped_to_decode_page_cap() {
    let cap = crate::line::MAX_DECODE_PAGE_LINES;
    let huge = cap.saturating_mul(4);
    let sb = Scrollback::with_block_size(huge, huge, usize::MAX, huge);
    assert_eq!(
        sb.block_size(),
        cap,
        "block_size clamped to the decode-page cap"
    );
}

#[test]
fn hot_tier_take_front() {
    let mut hot = HotTier::new();
    for i in 0..10 {
        hot.push(Line::from(&*format!("Line {i}")));
    }

    let taken = hot.take_front(3);
    assert_eq!(taken.len(), 3);
    assert_eq!(taken[0].to_string(), "Line 0");
    assert_eq!(taken[2].to_string(), "Line 2");
    assert_eq!(hot.len(), 7);
    assert_eq!(hot.get(0).unwrap().to_string(), "Line 3");
}

#[test]
fn hot_tier_truncate_front() {
    let mut hot = HotTier::new();
    for i in 0..10 {
        hot.push(Line::from(&*format!("Line {i}")));
    }

    hot.truncate_front(3);
    assert_eq!(hot.len(), 3);
    assert_eq!(hot.get(0).unwrap().to_string(), "Line 7");
}

// Warm tier tests
#[test]
fn warm_block_roundtrip() {
    let lines: Vec<Line> = (0..10).map(|i| Line::from(&*format!("Line {i}"))).collect();

    let block = WarmBlock::from_lines(&lines);
    assert_eq!(block.line_count(), 10);
    assert!(block.compressed_size() > 0);

    let decompressed = block.decompress().expect("no error");
    assert_eq!(decompressed.len(), 10);
    assert_eq!(decompressed[0].to_string(), "Line 0");
    assert_eq!(decompressed[9].to_string(), "Line 9");
}

#[test]
fn warm_block_get_line() {
    let lines: Vec<Line> = (0..10).map(|i| Line::from(&*format!("Line {i}"))).collect();

    let block = WarmBlock::from_lines(&lines);
    assert_eq!(
        block
            .get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
    assert_eq!(
        block
            .get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 5"
    );
    assert!(block.get_line(10).expect("no error").is_none());
}

#[test]
fn warm_tier_push_get() {
    let mut warm = WarmTier::new();

    let lines1: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block0-Line{i}")))
        .collect();
    let lines2: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block1-Line{i}")))
        .collect();

    warm.push_block(&lines1);
    warm.push_block(&lines2);

    assert_eq!(warm.line_count(), 10);
    assert_eq!(warm.block_count(), 2);

    // Test access across blocks
    assert_eq!(
        warm.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block0-Line0"
    );
    assert_eq!(
        warm.get_line(4)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block0-Line4"
    );
    assert_eq!(
        warm.get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line0"
    );
    assert_eq!(
        warm.get_line(9)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line4"
    );
    assert!(warm.get_line(10).expect("no error").is_none());
}

#[test]
fn warm_tier_pop_front() {
    let mut warm = WarmTier::new();

    let lines1: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block0-Line{i}")))
        .collect();
    let lines2: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block1-Line{i}")))
        .collect();

    warm.push_block(&lines1);
    warm.push_block(&lines2);

    let block = warm.pop_front().unwrap();
    assert_eq!(block.line_count(), 5);
    assert_eq!(warm.line_count(), 5);
    assert_eq!(
        warm.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line0"
    );
}

/// Regression test for W1-576: `memory_used()` must include the
/// decompression cache (`last_block_cache`). Without the cache term,
/// `memory_used()` under-reports when lines have been accessed via
/// `get_line()`.
#[test]
fn warm_tier_memory_used_includes_cache() {
    let mut warm = WarmTier::new();

    let lines: Vec<Line> = (0..50)
        .map(|i| Line::from(&*format!("Cache-test-line-{i}")))
        .collect();
    warm.push_block(&lines);

    let mem_before_access = warm.memory_used();
    // Trigger decompression cache by accessing a line.
    let line = warm
        .get_line(0)
        .expect("no error")
        .expect("get_line(0) should return data for a pushed block");
    assert_eq!(line.to_string(), "Cache-test-line-0");
    let mem_after_access = warm.memory_used();

    assert!(
        mem_after_access > mem_before_access,
        "memory_used should increase after cache is populated: before={mem_before_access}, after={mem_after_access}"
    );
}

/// Verify warm-tier block lookup scales logarithmically with block count.
#[test]
fn warm_tier_find_block_logarithmic_scaling() {
    fn measure_find_block_steps(block_count: usize) -> usize {
        let mut warm = WarmTier::new();
        for i in 0..block_count {
            warm.push_block(&[Line::from(&*format!("Warm-line-{i}"))]);
        }

        assert_eq!(warm.block_count(), block_count);
        assert_eq!(warm.line_count(), block_count);

        // Clear any prior counter state from earlier tests.
        let _ = take_warm_find_block_steps();

        let found = warm.find_block(block_count - 1);
        assert_eq!(found, Some(block_count - 1));

        take_warm_find_block_steps()
    }

    let small_steps = measure_find_block_steps(64);
    let large_steps = measure_find_block_steps(4096);

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
        "warm find_block should grow sublinearly: small={small_steps}, large={large_steps}"
    );
}

/// Regression test: push_front restores a popped block to the front.
#[test]
fn warm_tier_push_front_restores_popped_block() {
    let mut warm = WarmTier::new();

    let lines1: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block0-Line{i}")))
        .collect();
    let lines2: Vec<Line> = (0..5)
        .map(|i| Line::from(&*format!("Block1-Line{i}")))
        .collect();

    warm.push_block(&lines1);
    warm.push_block(&lines2);
    assert_eq!(warm.line_count(), 10);
    assert_eq!(warm.block_count(), 2);

    // Pop then push_front should restore the tier to its original state.
    let block = warm.pop_front().unwrap();
    assert_eq!(warm.line_count(), 5);
    assert_eq!(warm.block_count(), 1);

    warm.push_front(block);
    assert_eq!(warm.line_count(), 10);
    assert_eq!(warm.block_count(), 2);

    // Verify data integrity — block0 should be at the front again.
    assert_eq!(
        warm.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block0-Line0"
    );
    assert_eq!(
        warm.get_line(4)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block0-Line4"
    );
    assert_eq!(
        warm.get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line0"
    );
    assert_eq!(
        warm.get_line(9)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Block1-Line4"
    );
}

/// Exact rebuild-work proof for the high-block warm tiers used in budget tests.
///
/// The `warm_eviction_cost_bounded_with_many_blocks` wall-clock guard fills a
/// `Scrollback` with `block_size=5`, then slashes the memory budget. This test
/// reproduces that configured setup, isolates the resulting `WarmTier`, and
/// proves that repeated `pop_front()` calls perform exactly
/// `B * (B - 1) / 2` cumulative index rebuild steps for the warm-block counts
/// this high-block test shape creates.
#[test]
fn warm_tier_pop_front_rebuild_steps_match_quadratic_bound() {
    fn measure_rebuild_steps(warm_limit: usize) -> (usize, usize) {
        let mut sb = Scrollback::with_block_size(5, warm_limit, 500_000_000, 5);
        let fill = warm_limit + 200;
        for i in 0..fill {
            sb.push_str(&format!("Warm-step-line-{i}"));
        }

        let warm_blocks = sb.warm.block_count();
        assert!(warm_blocks > 0, "need warm blocks for rebuild-step proof");
        assert!(
            sb.cold_line_count() > 0,
            "fill pattern should match the end-to-end memory-pressure setup"
        );

        let mut rebuild_steps = 0usize;
        while sb.warm.pop_front().is_some() {
            rebuild_steps += sb.warm.cumulative_lines.len();
        }

        assert_eq!(
            sb.warm.block_count(),
            0,
            "all warm blocks should be evicted"
        );
        (warm_blocks, rebuild_steps)
    }

    let (small_blocks, small_steps) = measure_rebuild_steps(200);
    let (large_blocks, large_steps) = measure_rebuild_steps(2000);

    let small_expected = small_blocks.saturating_mul(small_blocks.saturating_sub(1)) / 2;
    let large_expected = large_blocks.saturating_mul(large_blocks.saturating_sub(1)) / 2;

    assert_eq!(
        small_steps, small_expected,
        "small warm eviction should rebuild cumulative index exactly B*(B-1)/2 times"
    );
    assert_eq!(
        large_steps, large_expected,
        "large warm eviction should rebuild cumulative index exactly B*(B-1)/2 times"
    );
    assert!(
        large_steps > small_steps,
        "larger warm tier should perform more rebuild steps"
    );
}

/// Verify that WarmBlock rejects LZ4 data with a forged oversized size prefix (#3598).
#[test]
fn warm_block_decompress_rejects_oversized_lz4_prefix() {
    // Forge LZ4 data with a size prefix claiming 128MB output
    let mut forged = Vec::new();
    let huge_size: u32 = 128 * 1024 * 1024;
    forged.extend_from_slice(&huge_size.to_le_bytes());
    forged.extend_from_slice(&[0u8; 10]); // garbage compressed data

    let block = WarmBlock {
        compressed: forged,
        line_count: 1,
        decompress_failures: Cell::new(0),
    };

    let result = block.decompress();
    assert!(result.is_err(), "oversized LZ4 prefix should be rejected");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("exceeds"),
        "error should mention size limit: {err_msg}"
    );
}

/// Verify that to_cold_compressed rejects forged LZ4 data (#3598).
#[test]
fn warm_block_to_cold_rejects_oversized_lz4_prefix() {
    let mut forged = Vec::new();
    let huge_size: u32 = 128 * 1024 * 1024;
    forged.extend_from_slice(&huge_size.to_le_bytes());
    forged.extend_from_slice(&[0u8; 10]);

    let block = WarmBlock {
        compressed: forged,
        line_count: 1,
        decompress_failures: Cell::new(0),
    };

    let result = block.to_cold_compressed();
    assert!(
        result.is_err(),
        "to_cold_compressed should reject oversized LZ4 prefix"
    );
}
