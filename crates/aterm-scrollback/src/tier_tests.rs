// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for tiered storage (hot/warm/cold).

use super::*;
use crate::HotTier;
use crate::HyperlinkSpan;
use crate::Scrollback;
use std::cell::Cell;

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

    /// Mutable access to the underlying block deque for test corruption.
    pub(crate) fn blocks_mut(&mut self) -> &mut std::collections::VecDeque<WarmBlock> {
        &mut self.blocks
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

/// A configured block_size above the disk-decode line-count cap is clamped, so
/// no promoted block holds more lines than the decoder will reconstruct (a
/// legitimate oversized block would decode short and fail its count check).
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

/// `memory_used()` includes the decompression cache (`last_block_cache`).
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

/// Warm-tier block lookup scales logarithmically with block count.
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

/// push_front restores a popped block to the front.
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

/// WarmTier's amortized cumulative index (`cum_start` + `cumulative_base`)
/// answers `line_count()`, `get_line`, `locate` and `segment_len_at` exactly as
/// a fresh `rebuild_cumulative()` across mixed `push_block`, `push_front`,
/// `pop_front` and `truncate_front_lines`, including across the 64-block
/// compaction threshold.
#[test]
fn warm_tier_cumulative_index_matches_rebuild_across_front_and_back_ops() {
    fn assert_matches_rebuilt(warm: &WarmTier) {
        assert_eq!(warm.live_cumulative().len(), warm.block_count());
        assert_eq!(warm.memory_used(), warm.recompute_memory_used());

        // Clone the live blocks and front_offset into a reference tier that
        // rebuilds its cumulative index from scratch.
        let mut rebuilt = WarmTier::new();
        for block in &warm.blocks {
            rebuilt.push_front(WarmBlock {
                compressed: block.compressed.clone(),
                line_count: block.line_count,
                decompress_failures: Cell::new(block.decompress_failures.get()),
            });
        }
        // `push_front` in loop order reversed `blocks`; restore exact order.
        rebuilt.blocks.make_contiguous().reverse();
        rebuilt.front_offset = warm.front_offset;
        rebuilt.line_count = warm.line_count;
        rebuilt.rebuild_cumulative();

        assert_eq!(warm.line_count(), rebuilt.line_count());
        let physical_total = warm.line_count() + warm.front_offset;
        for phys in 0..=physical_total {
            assert_eq!(
                warm.locate(phys),
                rebuilt.locate(phys),
                "locate({phys}) mismatch (cum_start={}, base={})",
                warm.cum_start,
                warm.cumulative_base
            );
        }
        for idx in 0..=warm.line_count() {
            assert_eq!(
                warm.segment_len_at(idx),
                rebuilt.segment_len_at(idx),
                "segment_len_at({idx}) mismatch"
            );
            let a = warm.get_line(idx).expect("warm decode");
            let b = rebuilt.get_line(idx).expect("rebuilt decode");
            assert_eq!(
                a.as_ref().map(ToString::to_string),
                b.as_ref().map(ToString::to_string),
                "get_line({idx}) mismatch"
            );
        }
    }

    let mut warm = WarmTier::new();
    // 1. Push 90 variable-sized blocks (crosses the 64-block compaction floor).
    for b in 0..90usize {
        let count = (b % 5) + 1;
        let lines: Vec<Line> = (0..count)
            .map(|j| Line::from(&*format!("B{b}-L{j}")))
            .collect();
        warm.push_block(&lines);
    }
    assert_matches_rebuilt(&warm);

    // 2. Pop 70 blocks from the front (triggers amortized dead-prefix compaction
    //    once `cum_start >= 64` and `cum_start > live`).
    for step in 0..70usize {
        let popped = warm.pop_front().expect("block present");
        if step % 17 == 0 {
            // Interleave push_front restoration and re-pop.
            warm.push_front(popped);
            assert_matches_rebuilt(&warm);
            let _ = warm.pop_front().expect("restored block present");
        }
        if step % 10 == 0 || step >= 60 {
            assert_matches_rebuilt(&warm);
        }
    }

    // 3. Interleave partial and multi-block front truncations with new pushes.
    warm.truncate_front_lines(2).expect("partial truncate");
    assert_matches_rebuilt(&warm);

    let extra: Vec<Line> = (0..6).map(|j| Line::from(&*format!("Extra-{j}"))).collect();
    warm.push_block(&extra);
    assert_matches_rebuilt(&warm);

    warm.truncate_front_lines(11).expect("multi-block truncate");
    assert_matches_rebuilt(&warm);

    // 4. Pop after partial front_offset (materializes front_offset first).
    let popped = warm.pop_front().expect("pop after partial offset");
    assert_matches_rebuilt(&warm);
    warm.push_front(popped);
    assert_matches_rebuilt(&warm);
}

/// WarmBlock rejects LZ4 data with a forged oversized size prefix.
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

/// to_cold_compressed rejects forged LZ4 data.
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

/// A row whose every cell is its own OSC 8 link, alternating two URLs at the
/// 8 KiB ingestion cap: the most bytes an honest row of `cols` cells carries.
fn row_of_links(text: &str, cols: u16, urls: &[std::sync::Arc<str>; 2]) -> Line {
    let spans = (0..cols)
        .map(|col| HyperlinkSpan::new(col, col + 1, urls[usize::from(col % 2)].clone()))
        .collect();
    Line::with_hyperlinks(text, crate::Rle::new(), spans)
}

/// No warm block is stored past what its reader decodes (SB1,
/// docs/AUDIT-persisted-state-2026-10-07.md). Seventy 320-column rows of links
/// (~184 MB serialized, three pages) share a promotion batch with thirty plain
/// lines; aged through the warm tier into the cold one, every line must still
/// be there, readable at its index, links intact, and the warm tier must never
/// hold more than its limit while the batch's blocks leave it.
#[test]
fn rows_of_links_past_the_page_cap_age_out_whole() {
    const COLS: u16 = 320;
    let urls: [std::sync::Arc<str>; 2] = ["a", "b"].map(|fill| {
        let mut url = String::from("https://example.com/");
        url.push_str(&fill.repeat(8192 - url.len()));
        std::sync::Arc::from(url)
    });
    let linked = |i: usize| i % 10 >= 3 && i < 100;
    // Enough after the batch for its block to reach the front of the warm
    // tier and be evicted past the quarantine threshold's retries.
    let total = 1_100 + crate::codec::DEFAULT_HOT_LIMIT + crate::codec::DEFAULT_WARM_LIMIT;
    let mut sb = Scrollback::with_defaults();
    for i in 0..total {
        let text = format!("line {i:05}");
        sb.push_line(if linked(i) {
            row_of_links(&text, COLS, &urls)
        } else {
            Line::from(text.as_str())
        });
        assert!(
            sb.warm_line_count() <= crate::codec::DEFAULT_WARM_LIMIT,
            "warm tier over its limit after line {i}"
        );
    }

    assert_eq!(sb.line_count(), total, "no line was dropped");
    assert_eq!(sb.pressure_evicted_lines(), 0);
    assert!(
        sb.cold_line_count() >= 100,
        "the first batch aged into cold"
    );
    for i in 0..total {
        let line = sb.get_line(i).expect("decodes").expect("in range");
        assert_eq!(line.to_string(), format!("line {i:05}"));
        let links = line.hyperlinks().map_or(0, <[HyperlinkSpan]>::len);
        assert_eq!(
            links,
            if linked(i) { usize::from(COLS) } else { 0 },
            "line {i}"
        );
    }
}
