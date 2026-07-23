// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded decompression codec tests (#3598) and error-safety tests for
//! corrupt compressed data in warm/cold tiers (#4638, #5893, #5921).

use super::*;

// Bounded Decompression Tests (#3598)

#[test]
fn decompress_lz4_bounded_rejects_oversized_prefix() {
    let mut forged = (128u32 * 1024 * 1024).to_le_bytes().to_vec();
    forged.extend_from_slice(&[0u8; 10]);
    let err = decompress_lz4_bounded(&forged).expect_err("should reject oversized prefix");
    assert!(matches!(err, ScrollbackError::Decompression(ref m) if m.contains("exceeds")));
}

#[test]
fn decompress_lz4_bounded_rejects_too_short() {
    let err = decompress_lz4_bounded(&[0u8; 3]).expect_err("should reject short data");
    assert!(matches!(err, ScrollbackError::Decompression(ref m) if m.contains("too short")));
}

#[cfg(feature = "zstd")]
#[test]
fn decode_zstd_bounded_roundtrip() {
    let original = b"hello world scrollback data";
    let compressed = zstd::encode_all(&original[..], 3).expect("zstd encode");
    let decoded = decode_zstd_bounded(&compressed).expect("bounded decode");
    assert_eq!(&decoded, original);
}

#[test]
fn decode_cold_bounded_roundtrip() {
    let original = b"hello world scrollback data";
    let compressed = crate::encode_cold_block(original).expect("cold encode");
    let decoded = crate::decode_cold_bounded(&compressed).expect("bounded cold decode");
    assert_eq!(&decoded, original);
}

#[test]
fn decompress_lz4_bounded_roundtrip() {
    let original = b"hello world scrollback data";
    let compressed = crate::lz4::compress_prepend_size(original).expect("test data fits in u32");
    let decoded = decompress_lz4_bounded(&compressed).expect("bounded lz4 decode");
    assert_eq!(&decoded, original);
}

// Decompression Failure Behavioral Tests (#4638, #5893)

/// Truncate with corrupt warm block succeeds via front_offset (no decompression).
///
/// Before the front_offset optimization, truncation decompressed the boundary
/// block and failed on corrupt data. Now truncation advances `front_offset`
/// instead, making line-limit enforcement immune to corruption. The corrupt
/// block is detected later when accessed (get_line) or evicted (pop_front).
#[test]
fn truncate_succeeds_on_corrupt_warm_block() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert!(sb.warm_line_count() > 0, "need warm data");
    sb.warm.corrupt_oldest_block();
    let result = sb.truncate(12);
    assert!(
        result.is_ok(),
        "truncate should succeed via front_offset (no decompression needed)"
    );
    assert_eq!(sb.line_count(), 12, "line count should reflect truncation");
}

/// Three-tier truncate with corrupt warm block succeeds via front_offset.
///
/// Validates that truncation spanning cold+warm+hot tiers does not require
/// decompression of the warm boundary block. The front_offset pattern makes
/// all three tiers infallible during truncation.
#[test]
fn truncate_three_tier_succeeds_with_corrupt_warm_block() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold data");
    assert!(sb.warm_line_count() > 0, "need warm data");
    sb.warm.corrupt_oldest_block();
    // Keep 12: removes 13 → cold(5 all) + warm(8 from corrupt block).
    let result = sb.truncate(12);
    assert!(
        result.is_ok(),
        "truncate should succeed via front_offset (no warm decompression needed)"
    );
    assert_eq!(sb.line_count(), 12, "line count should reflect truncation");
}

/// Verify that `remove_newest` returns Err and leaves state unchanged when
/// a warm block has corrupt compressed data.
#[test]
fn remove_newest_aborts_on_decompression_failure() {
    // Small tiers: hot=5, warm=10, block_size=5
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 15 lines → 5 in warm, 10 in hot
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 15);
    assert!(sb.warm_line_count() > 0, "need warm data for this test");

    let line_count_before = sb.line_count();
    let hot_before = sb.hot_line_count();
    let warm_before = sb.warm_line_count();
    let cold_before = sb.cold_line_count();

    // Corrupt the oldest warm block
    sb.warm.corrupt_oldest_block();

    // Remove enough lines to require reading from warm tier
    // Remove 12 → keep 3 oldest lines (which are in corrupt warm block)
    let result = sb.remove_newest(12);
    assert!(
        result.is_err(),
        "remove_newest should fail on corrupt warm block"
    );

    // State must be unchanged
    assert_eq!(
        sb.line_count(),
        line_count_before,
        "line_count changed on error"
    );
    assert_eq!(sb.hot_line_count(), hot_before, "hot tier changed on error");
    assert_eq!(
        sb.warm_line_count(),
        warm_before,
        "warm tier changed on error"
    );
    assert_eq!(
        sb.cold_line_count(),
        cold_before,
        "cold tier changed on error"
    );
}

/// Verify that when cold tier rejects a warm block (Zstd re-compression failure),
/// the block is restored to warm tier instead of being silently dropped.
///
/// Before the fix, `evict_warm_to_cold` would pop a block from warm, call
/// `cold.push_block()` which returned 0 on error, then adjust line_count down —
/// permanently losing an entire block of lines. Now the block is pushed back to
/// warm, matching `DiskBackedScrollback::evict_warm_to_cold` behavior.
#[test]
fn evict_warm_to_cold_restores_block_on_failure() {
    // hot=5, warm=10, block_size=5: warm holds 2 blocks of 5 before eviction.
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 15 lines to fill hot (5) + warm (10, 2 blocks).
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 15);
    assert_eq!(sb.warm_line_count(), 10, "expect 2 warm blocks of 5 lines");
    assert_eq!(sb.cold_line_count(), 0, "no cold data yet");

    // Corrupt the oldest warm block (lines 0-4) so cold re-compression fails.
    sb.warm.corrupt_oldest_block();

    // Push 5 more lines to trigger another hot→warm promotion + eviction attempt.
    // The corrupt block eviction to cold fails, but now the block is restored to warm.
    for i in 15..20 {
        sb.push_str(&format!("Line {i}"));
    }

    // The critical invariant: line_count == hot + warm + cold.
    let actual_total = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        actual_total,
        "line_count desync: reported={}, actual hot+warm+cold={}",
        sb.line_count(),
        actual_total
    );

    // No lines should be lost — the corrupt block was restored to warm.
    assert_eq!(
        sb.line_count(),
        20,
        "all 20 lines should be preserved (corrupt block restored to warm)"
    );

    // The corrupt block (lines 0-4) returns decompression errors when read,
    // but it still occupies its slot in warm tier (not silently dropped).
    // Non-corrupt lines (5+) should be readable.
    let result = sb.get_line(0);
    assert!(
        result.is_err(),
        "corrupt warm block should return decompression error, not silently vanish"
    );

    // Lines beyond the corrupt block should still be accessible.
    let last_idx = sb.line_count() - 1;
    let result = sb.get_line(last_idx);
    assert!(
        result.is_ok(),
        "non-corrupt line {last_idx} should be readable: {:?}",
        result.err()
    );

    // Out-of-range should return None, not error.
    assert!(sb.get_line(sb.line_count()).expect("no error").is_none());
}

/// Truncation into warm leaves a logical front_offset. If that partially
/// consumed front block later turns corrupt, eviction retries must preserve the
/// surviving logical lines until the quarantine threshold is hit.
#[test]
fn evict_warm_to_cold_preserves_trimmed_corrupt_block_until_quarantine() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }

    sb.truncate(12).expect("truncate should succeed");
    assert_eq!(sb.line_count(), 12, "truncate should keep 12 newest lines");

    sb.warm.corrupt_oldest_block();

    for i in 15..20 {
        sb.push_str(&format!("Line {i}"));
    }

    let actual_total = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        actual_total,
        "line_count must stay in sync after first failed eviction retry"
    );
    assert_eq!(
        sb.line_count(),
        17,
        "no live lines should be lost before the warm block is quarantined"
    );
    assert!(
        sb.get_line(0).is_err(),
        "trimmed corrupt block should remain readable only as an error until quarantine"
    );

    let extra_batches = usize::from(crate::tier::QUARANTINE_THRESHOLD) - 1;
    let mut next_line = 20;
    for _ in 0..extra_batches {
        for _ in 0..5 {
            sb.push_str(&format!("Line {next_line}"));
            next_line += 1;
        }
    }

    let actual_total = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        actual_total,
        "line_count must stay in sync after quarantine removes the surviving suffix"
    );
    assert_eq!(
        sb.line_count(),
        25,
        "quarantine should drop only the 2 surviving logical lines from the corrupt block"
    );
    let first = sb
        .get_line(0)
        .expect("no error after quarantine")
        .expect("first line should remain present after quarantine");
    assert_eq!(
        first.to_string(),
        "Line 5",
        "line 3 and line 4 were the only remaining lines in the corrupt trimmed block"
    );
}

/// Regression test (#5921, #6070): the budget setter must surface rejected
/// warm-to-cold eviction instead of looping or reporting success.
#[test]
fn set_memory_budget_reports_cold_rejection() {
    // Small budget forces memory pressure. block_size=5, warm_limit=5 (1 block).
    let mut sb = Scrollback::with_block_size(5, 5, 200, 5);

    // Push enough lines to fill hot + warm (10 lines = 2 blocks).
    for i in 0..10 {
        sb.push_str(&format!("Pressure-line-{i}"));
    }
    assert!(
        sb.warm.block_count() > 0,
        "need warm data for eviction path"
    );

    // Corrupt warm block so cold tier rejects eviction.
    sb.warm.corrupt_oldest_block();
    let warm_before = sb.warm.block_count();
    // Trigger memory pressure — before #5921 fix this looped forever, and
    // before #6070 the setter still reported success after restoring the block.
    let error = sb
        .set_memory_budget(1)
        .expect_err("corrupt warm block should reject budget enforcement");
    assert!(
        matches!(error, ScrollbackError::EnforcementFailed { .. }),
        "expected enforcement failure, got {error:?}"
    );
    // Verify eviction was attempted and rejected (not just that the loop terminated).
    assert_eq!(
        sb.warm.block_count(),
        warm_before,
        "warm unchanged after rejection"
    );
    assert_eq!(
        sb.cold_line_count(),
        0,
        "cold empty — corrupt data rejected"
    );
    assert_eq!(
        sb.memory_budget(),
        1,
        "configured budget should still update after enforcement failure"
    );
}

// Warm Block Quarantine Tests (#5947)

/// Corrupt warm block is quarantined (dropped) after QUARANTINE_THRESHOLD
/// consecutive decompression failures during eviction attempts.
#[test]
fn corrupt_warm_block_quarantined_after_threshold() {
    // warm_limit=5 means each hot→warm promotion triggers one eviction attempt
    // when warm already has a block. This gives deterministic one-failure-per-batch.
    // hot=5, warm_limit=5, budget=10MB (large, not the eviction driver), block_size=5.
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);

    // Push 10 lines → 5 warm (1 block), 5 hot.
    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 5);
    assert_eq!(sb.warm.block_count(), 1);

    // Corrupt the warm block (lines 0-4).
    sb.warm.corrupt_oldest_block();

    // Push batches of 5 to trigger hot→warm promotion + eviction.
    // Each promotion pushes a new block, warm exceeds warm_limit (5),
    // and evict_warm_to_cold pops the corrupt front block → fails → pushed back.
    for batch in 0..crate::tier::QUARANTINE_THRESHOLD {
        for i in 0..5 {
            sb.push_str(&format!("Batch{batch}_{i}"));
        }
    }

    // After QUARANTINE_THRESHOLD eviction failures, the corrupt block is quarantined.
    // Its 5 lines are subtracted from total. The 3 healthy blocks (15 lines)
    // + remaining hot lines are all accessible.
    let tier_sum = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        tier_sum,
        "line_count must equal hot+warm+cold after quarantine"
    );

    // The 5 corrupt lines should be gone from the total.
    // Original 10 + 15 pushed - 5 quarantined = 20
    assert_eq!(
        sb.line_count(),
        20,
        "quarantined block's 5 lines should be subtracted"
    );
}

/// Each failed warm→cold eviction should count as exactly one failed decode.
/// The corrupt block must survive attempts below the quarantine threshold and
/// only disappear on the final threshold attempt.
#[test]
fn corrupt_warm_block_quarantines_only_on_final_eviction_attempt() {
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);

    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    sb.warm.corrupt_oldest_block();

    let threshold = usize::from(crate::tier::QUARANTINE_THRESHOLD);
    for attempt in 1..=threshold {
        for i in 0..5 {
            sb.push_str(&format!("Attempt{attempt}_{i}"));
        }

        let tier_sum = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
        assert_eq!(
            sb.line_count(),
            tier_sum,
            "line_count must stay in sync after eviction attempt {attempt}"
        );

        if attempt < threshold {
            assert_eq!(
                sb.line_count(),
                10 + (attempt * 5),
                "failed eviction attempt {attempt} must not drop the corrupt block early"
            );
            // Avoid probing `get_line()` here: read-path failures also advance
            // the quarantine counter, which would perturb the eviction-only
            // accounting this regression is checking.
            assert!(
                sb.cold_line_count() == 0,
                "attempt {attempt} must not silently accept the corrupt block into cold storage"
            );
        } else {
            assert_eq!(
                sb.line_count(),
                20,
                "threshold attempt must drop exactly the corrupt block's 5 lines"
            );
            let first = sb
                .get_line(0)
                .expect("oldest surviving line should be readable after quarantine")
                .expect("line 5 should remain after the corrupt block is dropped");
            assert_eq!(
                first.to_string(),
                "Line 5",
                "the corrupt block should disappear only on the final threshold attempt"
            );
        }
    }
}

/// Iterator skips quarantined warm block lines without redundant decompression.
/// Before the fix, each line in a corrupt block triggered a separate decompress
/// attempt, causing O(B) wasted work per iteration pass.
#[test]
fn iterator_skips_quarantined_block_without_redundant_decompression() {
    // Large budget so we don't trigger eviction — we want to test get_line
    // on a block that's manually quarantined.
    let mut sb = Scrollback::with_block_size(5, 100, 10_000_000, 5);

    // Push 15 lines → 10 in warm (2 blocks of 5), 5 in hot.
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 10);
    let non_corrupt_count = 10; // 5 warm (block 2) + 5 hot

    // Corrupt the oldest warm block (lines 0-4).
    sb.warm.corrupt_oldest_block();

    // Manually quarantine the block by recording QUARANTINE_THRESHOLD failures.
    // We access the front block directly through the test helper.
    for _ in 0..crate::tier::QUARANTINE_THRESHOLD {
        if let Some(block) = sb.warm.blocks_mut().front_mut() {
            block.record_failure();
        }
    }

    // Iterator should skip the quarantined block's 5 lines and yield
    // only the 10 non-quarantined lines (5 warm block 2 + 5 hot).
    let lines: Vec<_> = sb.iter().map(|l| l.to_string()).collect();
    assert_eq!(
        lines.len(),
        non_corrupt_count,
        "iterator should skip quarantined block lines"
    );

    // Verify the yielded lines are the non-corrupt ones (lines 5-14).
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            *line,
            format!("Line {}", i + 5),
            "line at position {i} should be from non-corrupt blocks"
        );
    }
}

/// Line limit enforcement succeeds after a corrupt warm block is quarantined.
/// Before the fix, truncation failed permanently at the corrupt block boundary.
#[test]
fn line_limit_enforced_past_corrupt_block() {
    // Same warm_limit=5 pattern: one eviction attempt per promotion.
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);

    // Push 10 lines → 5 warm (1 block), 5 hot.
    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 5);

    // Corrupt the warm block.
    sb.warm.corrupt_oldest_block();

    // Trigger quarantine through 3 eviction failures (5 lines per batch).
    for batch in 0..crate::tier::QUARANTINE_THRESHOLD {
        for i in 0..5 {
            sb.push_str(&format!("B{batch}_{i}"));
        }
    }

    // Corrupt block is now quarantined. Set a line limit.
    sb.set_line_limit(Some(10));

    // Push more lines to test that the limit is actually enforced.
    for i in 0..5 {
        sb.push_str(&format!("Extra {i}"));
    }
    assert!(
        sb.line_count() <= 10,
        "line limit should be enforced after quarantine, got {}",
        sb.line_count()
    );
}
