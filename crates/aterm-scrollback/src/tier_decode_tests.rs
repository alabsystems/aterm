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
        encoded_line_count: 0,
        decompress_failures: Cell::new(0),
    };

    let err = block
        .decompress()
        .expect_err("truncated serialized payload must not masquerade as a repaired suffix");
    assert!(
        matches!(&err, crate::ScrollbackError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData),
        "{err:?}"
    );
}

#[test]
fn cold_transfer_reuses_a_valid_noncanonical_lz4_page() {
    let lines: Vec<Line> = (0..100).map(|_| Line::from("repeated content")).collect();
    let serialized = serialize_lines(&lines);
    // A valid literal-only LZ4 block differs from the encoder's compressed
    // representation, so equality also detects accidental recompression.
    let mut compressed = u32::try_from(serialized.len())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    compressed.push(0xf0);
    let mut remaining = serialized.len() - 15;
    while remaining >= 255 {
        compressed.push(255);
        remaining -= 255;
    }
    compressed.push(u8::try_from(remaining).unwrap());
    compressed.extend_from_slice(&serialized);
    let mut block = WarmBlock {
        compressed: compressed.clone(),
        line_count: lines.len(),
        encoded_line_count: 0,
        decompress_failures: Cell::new(2),
    };

    let original = block.compressed.as_ptr();
    let (cold, count) = block.take_cold_compressed().expect("valid page");
    assert_eq!(
        cold.as_ptr(),
        original,
        "transfer moves the validated allocation"
    );
    assert!(block.compressed.is_empty());
    assert_eq!(block.line_count(), 0);
    assert_eq!(cold, compressed);
    assert_eq!(count, lines.len());
    assert_eq!(block.failure_count(), 0);
    assert_eq!(crate::decode_cold_bounded(&cold).unwrap(), serialized);
}

#[test]
fn cold_transfer_discards_a_logically_trimmed_prefix() {
    let lines: Vec<Line> = (0..9).map(|i| Line::from(&*format!("line {i}"))).collect();
    let mut block = WarmBlock::from_lines(&lines);
    block.line_count = 4;
    let (cold, count) = block.take_cold_compressed().expect("valid suffix");
    assert_eq!(count, 4);
    assert_eq!(
        crate::decode_cold_bounded(&cold).unwrap(),
        serialize_lines(&lines[5..])
    );
}

#[test]
fn cold_transfer_validates_framing_before_reusing_compressed_bytes() {
    let lines = [Line::from("one"), Line::from("two")];
    let mut serialized = serialize_lines(&lines);
    serialized.truncate(serialized.len() - 1);
    let mut block = WarmBlock {
        compressed: crate::lz4::compress_prepend_size(&serialized).unwrap(),
        line_count: lines.len(),
        encoded_line_count: 0,
        decompress_failures: Cell::new(0),
    };
    for expected in 1..=QUARANTINE_THRESHOLD {
        let original = block.compressed.clone();
        assert!(block.take_cold_compressed().is_err());
        assert_eq!(block.compressed, original);
        assert_eq!(block.line_count(), lines.len());
        assert_eq!(block.failure_count(), expected);
    }
    assert!(block.is_quarantined());
}

/// `WarmTier::get_line()` failures alone advance a corrupt block to quarantine:
/// read-path callers do not silently tolerate persistent corruption.
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
