// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded decompression and corrupt-block error-safety tests for warm and cold tiers.

use super::*;

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

#[test]
fn truncate_three_tier_succeeds_with_corrupt_warm_block() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold data");
    assert!(sb.warm_line_count() > 0, "need warm data");
    sb.warm.corrupt_oldest_block();
    let result = sb.truncate(12);
    assert!(
        result.is_ok(),
        "truncate should succeed via front_offset (no warm decompression needed)"
    );
    assert_eq!(sb.line_count(), 12, "line count should reflect truncation");
}

#[test]
fn remove_newest_aborts_on_decompression_failure() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 15);
    assert!(sb.warm_line_count() > 0, "need warm data for this test");

    let line_count_before = sb.line_count();
    let hot_before = sb.hot_line_count();
    let warm_before = sb.warm_line_count();
    let cold_before = sb.cold_line_count();

    sb.warm.corrupt_oldest_block();

    let result = sb.remove_newest(12);
    assert!(
        result.is_err(),
        "remove_newest should fail on corrupt warm block"
    );

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

#[test]
fn evict_warm_to_cold_restores_block_on_failure() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 15);
    assert_eq!(sb.warm_line_count(), 10, "expect 2 warm blocks of 5 lines");
    assert_eq!(sb.cold_line_count(), 0, "no cold data yet");

    sb.warm.corrupt_oldest_block();

    for i in 15..20 {
        sb.push_str(&format!("Line {i}"));
    }

    let actual_total = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        actual_total,
        "line_count desync: reported={}, actual hot+warm+cold={}",
        sb.line_count(),
        actual_total
    );

    assert_eq!(
        sb.line_count(),
        20,
        "all 20 lines should be preserved (corrupt block restored to warm)"
    );

    let result = sb.get_line(0);
    assert!(
        result.is_err(),
        "corrupt warm block should return decompression error, not silently vanish"
    );

    let last_idx = sb.line_count() - 1;
    let result = sb.get_line(last_idx);
    assert!(
        result.is_ok(),
        "non-corrupt line {last_idx} should be readable: {:?}",
        result.err()
    );

    assert!(sb.get_line(sb.line_count()).expect("no error").is_none());
}

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

#[test]
fn set_memory_budget_reports_cold_rejection() {
    let mut sb = Scrollback::with_block_size(5, 5, 200, 5);

    for i in 0..10 {
        sb.push_str(&format!("Pressure-line-{i}"));
    }
    assert!(
        sb.warm.block_count() > 0,
        "need warm data for eviction path"
    );

    sb.warm.corrupt_oldest_block();
    let warm_before = sb.warm.block_count();
    let error = sb
        .set_memory_budget(1)
        .expect_err("corrupt warm block should reject budget enforcement");
    assert!(
        matches!(error, ScrollbackError::EnforcementFailed { .. }),
        "expected enforcement failure, got {error:?}"
    );
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

#[test]
fn corrupt_warm_block_quarantined_after_threshold() {
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);

    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 5);
    assert_eq!(sb.warm.block_count(), 1);

    sb.warm.corrupt_oldest_block();

    for batch in 0..crate::tier::QUARANTINE_THRESHOLD {
        for i in 0..5 {
            sb.push_str(&format!("Batch{batch}_{i}"));
        }
    }

    let tier_sum = sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        tier_sum,
        "line_count must equal hot+warm+cold after quarantine"
    );

    assert_eq!(
        sb.line_count(),
        20,
        "quarantined block's 5 lines should be subtracted"
    );
}

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

#[test]
fn quarantine_mid_history_keeps_rows_and_counts_the_loss() {
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.cold_line_count(), 5, "older history sits in cold");
    sb.warm.corrupt_oldest_block();
    let corrupt = 5..10;

    let mut pushed = 15;
    for _ in 0..crate::tier::QUARANTINE_THRESHOLD {
        for _ in 0..5 {
            sb.push_str(&format!("Line {pushed}"));
            pushed += 1;
        }
    }

    assert_eq!(sb.line_count(), pushed, "every row keeps its index");
    assert_eq!(sb.pressure_evicted_lines(), 5, "the lost text is counted");
    for i in 0..pushed {
        let line = sb.get_line(i).expect("decodes").expect("in range");
        let expected = if corrupt.contains(&i) {
            String::new()
        } else {
            format!("Line {i}")
        };
        assert_eq!(line.to_string(), expected, "line {i}");
    }

    let before = sb.line_count();
    sb.set_memory_budget(1).expect("the budget shrink enforces");
    assert_eq!(sb.cold_line_count(), 0, "no cold page fits one byte");
    let evicted = before - sb.line_count();
    assert!(evicted > 10, "the budget took blank rows and real text");
    assert_eq!(sb.pressure_evicted_lines(), evicted as u64);
}

#[test]
fn iterator_skips_quarantined_block_without_redundant_decompression() {
    let mut sb = Scrollback::with_block_size(5, 100, 10_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 10);
    let non_corrupt_count = 10; // 5 warm (block 2) + 5 hot

    sb.warm.corrupt_oldest_block();

    for _ in 0..crate::tier::QUARANTINE_THRESHOLD {
        if let Some(block) = sb.warm.blocks_mut().front_mut() {
            block.record_failure();
        }
    }

    let lines: Vec<_> = sb.iter().map(|l| l.to_string()).collect();
    assert_eq!(
        lines.len(),
        non_corrupt_count,
        "iterator should skip quarantined block lines"
    );

    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            *line,
            format!("Line {}", i + 5),
            "line at position {i} should be from non-corrupt blocks"
        );
    }
}

#[test]
fn line_limit_enforced_past_corrupt_block() {
    let mut sb = Scrollback::with_block_size(5, 5, 10_000_000, 5);

    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.warm_line_count(), 5);

    sb.warm.corrupt_oldest_block();

    for batch in 0..crate::tier::QUARANTINE_THRESHOLD {
        for i in 0..5 {
            sb.push_str(&format!("B{batch}_{i}"));
        }
    }

    sb.set_line_limit(Some(10));

    for i in 0..5 {
        sb.push_str(&format!("Extra {i}"));
    }
    assert!(
        sb.line_count() <= 10,
        "line limit should be enforced after quarantine, got {}",
        sb.line_count()
    );
}
