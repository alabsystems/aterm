// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::line::serialize_lines;
use std::cell::Cell;

#[test]
fn warm_block_successful_decompress_clears_failure_streak() {
    let lines: Vec<Line> = (0..3).map(|i| Line::from(&*format!("Line {i}"))).collect();
    let block = WarmBlock::from_lines(&lines);

    for _ in 0..usize::from(crate::tier::QUARANTINE_THRESHOLD.saturating_sub(1)) {
        assert!(
            !block.record_failure(),
            "block should stay below quarantine threshold before a successful decode"
        );
    }
    assert!(
        !block.is_quarantined(),
        "failure streak should stay below threshold before recovery"
    );

    let decoded = block.decompress().expect("healthy block should decompress");
    assert_eq!(
        decoded.len(),
        lines.len(),
        "successful decode should still return all lines"
    );
    assert!(
        !block.is_quarantined(),
        "successful decode should clear the old failure streak"
    );

    assert!(
        !block.record_failure(),
        "a new failure after recovery should start a fresh streak"
    );
    assert!(
        !block.is_quarantined(),
        "single post-recovery failure must not quarantine the block"
    );
}

#[test]
fn warm_block_decompress_rejects_truncated_serialized_payload() {
    let lines: Vec<Line> = (0..2).map(|i| Line::from(&*format!("Line {i}"))).collect();
    let mut serialized = serialize_lines(&lines);
    serialized[..4].copy_from_slice(&5u32.to_le_bytes());

    let block = WarmBlock {
        compressed: crate::lz4::compress_prepend_size(&serialized).expect("test data fits in u32"),
        line_count: 2,
        decompress_failures: Cell::new(0),
    };

    let err = block
        .decompress()
        .expect_err("truncated serialized payload must not masquerade as a repaired suffix");
    let err_text = format!("{err}");
    assert!(
        err_text.contains("decoded 2 complete lines"),
        "error should explain the serialized/header mismatch: {err_text}"
    );
}

/// Proves that `WarmTier::get_line()` failures advance a corrupt block toward
/// quarantine without using `record_failure()` directly. This is the critical
/// behavioral contract from #5947: read-path callers must not silently tolerate
/// persistent corruption.
#[test]
fn read_failures_reach_quarantine_without_manual_record_failure() {
    let mut warm = WarmTier::new();
    let lines: Vec<Line> = (0..5).map(|i| Line::from(&*format!("L{i}"))).collect();
    warm.push_block(&lines);

    // Corrupt the compressed data so decompress fails.
    warm.blocks[0].compressed = vec![0xFF; 8]; // invalid LZ4

    // Each get_line call should fail and increment the counter.
    for i in 0..usize::from(QUARANTINE_THRESHOLD) {
        let err = warm
            .get_line(0)
            .expect_err(&format!("attempt {i}: corrupt block should fail decode"));
        assert!(
            !matches!(err, crate::ScrollbackError::Quarantined(_)),
            "attempt {i}: should be a decode error, not yet quarantined"
        );
    }

    // Next get_line should see is_quarantined() == true.
    let err = warm
        .get_line(0)
        .expect_err("quarantined block should not retry");
    assert!(
        matches!(err, crate::ScrollbackError::Quarantined(_)),
        "after {QUARANTINE_THRESHOLD} decode failures, block should be quarantined"
    );
}
