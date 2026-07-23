// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for DiskBackedScrollback.
//!
//! Extracted from tests.rs (#2100).

use super::*;

#[test]
fn disk_backed_scrollback_cold_memory_used() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // cold_memory_used returns metadata/cache size for disk-backed cold tier
    let initial_cold_mem = sb.cold_memory_used();

    // Push 25 lines - should evict to disk-backed cold tier
    for i in 0..25 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    // Verify cold tier has data
    assert!(sb.cold_line_count() > 0);
    // Disk usage should be > 0 (this is where the compressed data lives)
    assert!(sb.cold_disk_used() > 0);
    // cold_memory_used includes metadata/index overhead for disk tier
    assert!(
        sb.cold_memory_used() >= initial_cold_mem,
        "cold memory should include index metadata"
    );

    let before_cache = sb.cold_memory_used();
    let line = sb
        .get_line(0)
        .expect("no error")
        .expect("get_line(0) should return data for a pushed block");
    assert_eq!(line.to_string(), "Line 0");
    let after_cache = sb.cold_memory_used();
    assert!(
        after_cache > before_cache,
        "cold memory should grow when cache is populated"
    );
}

#[test]
fn disk_backed_scrollback_total_memory_used() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // memory_used includes base struct size, so initial is non-zero
    let initial_total = sb.total_memory_used();

    // Push lines only to hot tier
    for i in 0..4 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }
    // Hot only: total = hot+warm + cold metadata
    let hot_warm = sb.memory_used();
    let cold_mem = sb.cold_memory_used();
    assert_eq!(sb.total_memory_used(), hot_warm + cold_mem);

    // Push 25 lines - should have data in all tiers
    for i in 4..25 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    // Total memory = hot+warm + cold metadata (disk data not counted)
    let hot_warm = sb.memory_used();
    let cold_mem = sb.cold_memory_used();
    assert_eq!(sb.total_memory_used(), hot_warm + cold_mem);
    // Disk usage should be > 0 (separate from memory)
    assert!(sb.cold_disk_used() > 0, "cold disk usage should have data");
    assert!(
        sb.total_memory_used() > initial_total,
        "total should increase"
    );
}

#[test]
fn disk_backed_incremental_bytes_used_matches_recomputed() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("tracked-cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(10)
        .with_warm_limit(30)
        .with_memory_budget(20_000)
        .with_block_size(10);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..200 {
        sb.push_str(&format!("Tracked-line-{i:03}-{}", "x".repeat(24)))
            .unwrap();
        assert_eq!(
            sb.total_memory_used(),
            sb.recompute_total_memory_used(),
            "counter drift after push {i}",
        );
    }

    sb.remove_newest(40).expect("remove_newest should succeed");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after remove_newest",
    );

    sb.truncate(60).expect("truncate should succeed");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after truncate",
    );

    sb.get_line(0).expect("no error").expect("line present");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after cold-cache population",
    );
}

#[test]
fn disk_backed_truncate_medium_path_restores_tier_structure() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("truncate-medium.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..15 {
        sb.push_str(&format!("Line {i:02}")).unwrap();
    }
    assert_eq!(
        sb.cold_line_count(),
        0,
        "fixture should stay in hot+warm only"
    );

    let keep = 12;
    assert!(
        keep > sb.hot_line_count(),
        "fixture must bypass hot-only fast path"
    );
    assert!(
        keep <= sb.hot_line_count() + sb.warm_line_count(),
        "fixture must exercise truncate medium path"
    );

    sb.truncate(keep).expect("truncate should succeed");
    assert_eq!(sb.line_count(), keep, "total line count after truncate");
    assert!(
        sb.hot_line_count() <= sb.hot_limit(),
        "hot tier ({}) must not exceed hot_limit ({}) after truncate",
        sb.hot_line_count(),
        sb.hot_limit()
    );

    for i in 0..keep {
        let line = sb
            .get_line(i)
            .expect("no I/O error")
            .unwrap_or_else(|| panic!("line {i} should exist"));
        let expected_idx = 15 - keep + i;
        assert_eq!(line.to_string(), format!("Line {expected_idx:02}"));
    }
}

#[test]
fn disk_backed_truncate_slow_path_restores_tier_structure() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("truncate-slow.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..30 {
        sb.push_str(&format!("Line {i:02}")).unwrap();
    }
    assert!(
        sb.cold_line_count() > 0,
        "fixture must include cold-tier data"
    );

    let keep = 20;
    assert!(
        keep > sb.hot_line_count() + sb.warm_line_count(),
        "fixture must exercise truncate slow path"
    );

    sb.truncate(keep).expect("truncate should succeed");
    assert_eq!(sb.line_count(), keep, "total line count after truncate");
    assert!(
        sb.hot_line_count() <= sb.hot_limit(),
        "hot tier ({}) must not exceed hot_limit ({}) after truncate",
        sb.hot_line_count(),
        sb.hot_limit()
    );

    for i in 0..keep {
        let line = sb
            .get_line(i)
            .expect("no I/O error")
            .unwrap_or_else(|| panic!("line {i} should exist"));
        let expected_idx = 30 - keep + i;
        assert_eq!(line.to_string(), format!("Line {expected_idx:02}"));
    }
}

#[test]
fn disk_backed_remove_newest_slow_path_restores_tier_structure() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("remove-slow.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..30 {
        sb.push_str(&format!("Line {i:02}")).unwrap();
    }

    let remove = sb.hot_line_count() + 1;
    sb.remove_newest(remove)
        .expect("remove_newest should succeed");

    let expected = 30 - remove;
    assert_eq!(sb.line_count(), expected, "total line count after remove");
    assert!(
        sb.hot_line_count() < sb.hot_limit(),
        "hot tier ({}) must stay under hot_limit ({}) after slow-path rebuild",
        sb.hot_line_count(),
        sb.hot_limit()
    );

    for i in 0..expected {
        let line = sb
            .get_line(i)
            .expect("no I/O error")
            .unwrap_or_else(|| panic!("line {i} should exist"));
        assert_eq!(line.to_string(), format!("Line {i:02}"));
    }
}

// =========================================================================
// DiskBackedScrollback Tier-Aware remove_newest Tests (#5918)
// =========================================================================

/// Disk-backed remove_newest: hot-only fast path.
#[test]
fn disk_backed_remove_newest_hot_only() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("rm-hot.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(10)
        .with_warm_limit(50)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..5 {
        sb.push_str(&format!("H-{i}")).unwrap();
    }
    assert_eq!(sb.hot_line_count(), 5);

    sb.remove_newest(2).expect("should succeed");
    assert_eq!(sb.line_count(), 3);

    for i in 0..3 {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("H-{i}"));
    }
}

/// Disk-backed remove_newest: spans hot + warm tiers.
#[test]
fn disk_backed_remove_newest_spans_warm() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("rm-warm.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(50)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..20 {
        sb.push_str(&format!("W-{i}")).unwrap();
    }
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    assert!(warm > 0, "need warm tier data: hot={hot} warm={warm}");

    let remove = hot + 3;
    sb.remove_newest(remove).expect("should succeed");
    let expected = 20 - remove;
    assert_eq!(sb.line_count(), expected);

    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("W-{i}"));
    }
}

/// Disk-backed remove_newest: spans into cold (disk) tier.
#[test]
fn disk_backed_remove_newest_spans_cold() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("rm-cold.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..50 {
        sb.push_str(&format!("C-{i}")).unwrap();
    }
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    let cold = sb.cold_line_count();
    assert!(
        cold > 0,
        "need cold tier: hot={hot} warm={warm} cold={cold}"
    );

    let remove = hot + warm + 3;
    sb.remove_newest(remove).expect("should succeed");
    let expected = 50 - remove;
    assert_eq!(sb.line_count(), expected);

    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("C-{i}"));
    }
}

/// Disk-backed remove_newest: partial warm block boundary.
#[test]
fn disk_backed_remove_newest_partial_warm_block() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("rm-partial.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(50)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..15 {
        sb.push_str(&format!("P-{i}")).unwrap();
    }
    let hot = sb.hot_line_count();
    assert!(sb.warm_line_count() > 0, "need warm data");

    let remove = hot + 2; // Leaves partial warm block
    sb.remove_newest(remove).expect("should succeed");
    let expected = 15 - remove;
    assert_eq!(sb.line_count(), expected);

    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("P-{i}"));
    }
}

/// Disk-backed remove_newest: all lines clears everything.
#[test]
fn disk_backed_remove_newest_all_lines() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("rm-all.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..30 {
        sb.push_str(&format!("A-{i}")).unwrap();
    }
    assert!(sb.cold_line_count() > 0);

    sb.remove_newest(30).expect("should succeed");
    assert_eq!(sb.line_count(), 0);
    assert_eq!(sb.hot_line_count(), 0);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);
}

/// #5918: Disk-backed remove_newest(1) on a large scrollback does NOT spike memory.
///
/// Same property as the in-memory test, but exercises DiskColdTier::truncate_back_lines
/// and the I/O path for boundary page re-write.
#[test]
fn disk_backed_remove_newest_memory_bounded_cross_tier() {
    use aterm_tempfile::tempdir;
    let dir = tempdir().unwrap();
    let path = dir.path().join("mem-bounded.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(200)
        .with_block_size(50);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // 1000 lines is enough to populate all three tiers (hot=5, warm=200,
    // cold=~800 in ~16 blocks) while keeping disk I/O test time under 2s.
    let total = 1_000;
    for i in 0..total {
        sb.push_str(&format!("DiskMem-{i}")).unwrap();
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");
    assert!(sb.warm_line_count() > 0, "need warm tier data");

    // Drain hot to force cross-tier path.
    let hot = sb.hot_line_count();
    if hot > 0 {
        sb.remove_newest(hot).expect("drain hot");
    }
    assert_eq!(sb.hot_line_count(), 0);

    let mem_before = sb.total_memory_used();
    let lines_before = sb.line_count();

    sb.remove_newest(1)
        .expect("remove_newest(1) should succeed");
    assert_eq!(sb.line_count(), lines_before - 1);

    let mem_after = sb.total_memory_used();
    let ratio = mem_after as f64 / mem_before.max(1) as f64;
    eprintln!(
        "disk remove_newest(1) memory: before={mem_before}, after={mem_after}, ratio={ratio:.2}x"
    );
    assert!(
        ratio < 1.5,
        "disk remove_newest(1) memory ratio {ratio:.2}x exceeds 1.5x — \
         likely materializing all lines (expected O(block_size), got O(total))"
    );
}

// =========================================================================
// DiskBackedScrollback Line Limit Tests (#1865)
// =========================================================================

#[test]
fn disk_backed_scrollback_line_limit_default_is_safe_cap() {
    // #7929: disk-backed storage must default to a bounded line limit so a
    // runaway stdout cannot fill the disk.
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path);
    assert_eq!(config.line_limit, Some(DEFAULT_LINE_LIMIT));

    let sb = DiskBackedScrollback::with_config(config).unwrap();
    assert_eq!(sb.line_limit(), Some(DEFAULT_LINE_LIMIT));
}

#[test]
fn disk_backed_scrollback_opt_in_unlimited_lines() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path).with_unlimited_lines();
    let sb = DiskBackedScrollback::with_config(config).unwrap();
    assert_eq!(sb.line_limit(), None);
}

#[test]
fn disk_backed_scrollback_set_line_limit() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Start from the unlimited opt-in so the Some→None→Some matrix is
    // unambiguous even after #7929 flipped the default.
    let config = DiskBackedScrollbackConfig::new(&path).with_unlimited_lines();
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();
    assert_eq!(sb.line_limit(), None);

    sb.set_line_limit(Some(100));
    assert_eq!(sb.line_limit(), Some(100));

    sb.set_line_limit(None);
    assert_eq!(sb.line_limit(), None);
}

#[test]
fn disk_backed_scrollback_line_limit_enforced_on_push() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(100)
        .with_warm_limit(1000);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();
    sb.set_line_limit(Some(10));

    // Push 15 lines
    for i in 0..15 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    // Only 10 lines should remain (oldest 5 discarded)
    assert_eq!(sb.line_count(), 10);

    // Should have newest lines (5-14)
    let line = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist");
    assert!(
        line.to_string().contains("Line 5"),
        "oldest kept should be line 5, got: {line}",
    );
    let line = sb
        .get_line(9)
        .expect("no error")
        .expect("line 9 should exist");
    assert!(
        line.to_string().contains("Line 14"),
        "newest should be line 14, got: {line}",
    );
}

#[test]
fn disk_backed_scrollback_set_line_limit_truncates_immediately() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(100)
        .with_warm_limit(1000);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // Push 20 lines
    for i in 0..20 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }
    assert_eq!(sb.line_count(), 20);

    // Set limit to 10 - should truncate immediately
    sb.set_line_limit(Some(10));
    assert_eq!(sb.line_count(), 10);

    // Should keep newest 10 lines (10-19)
    let line = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist");
    assert!(
        line.to_string().contains("Line 10"),
        "oldest kept should be line 10"
    );
}

#[test]
fn disk_backed_scrollback_line_limit_zero_disables() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(100)
        .with_warm_limit(1000);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();
    sb.set_line_limit(Some(5));

    for i in 0..10 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }
    assert_eq!(sb.line_count(), 5);

    // Setting to Some(0) clears everything
    sb.set_line_limit(Some(0));
    assert_eq!(sb.line_count(), 0);
}

/// Regression test for #2254: evict_warm_to_cold must not silently drop lines.
///
/// After warm→cold eviction, `line_count` must equal the sum of per-tier counts
/// and every line must be retrievable by index.
#[test]
fn disk_backed_scrollback_evict_warm_cold_no_data_loss() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Small limits to force warm→cold eviction: hot=5, warm=10, block=5.
    // After 20 lines: hot should have ~5, warm ~10, cold ~5.
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    let total_lines = 25;
    for i in 0..total_lines {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    // line_count must equal per-tier sum.
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    let cold = sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        hot + warm + cold,
        "line_count ({}) != hot ({hot}) + warm ({warm}) + cold ({cold})",
        sb.line_count()
    );

    // Every line must be retrievable.
    for i in 0..sb.line_count() {
        let line = sb
            .get_line(i)
            .expect("no error")
            .expect("line should remain retrievable after warm/cold eviction");
        assert_eq!(line.to_string(), format!("Line {i}"));
    }

    // Verify first and last lines are correct.
    assert_eq!(
        sb.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
    assert_eq!(
        sb.get_line(sb.line_count() - 1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        format!("Line {}", total_lines - 1)
    );
}

// =========================================================================
// Tier-aware truncate performance test (#5911)
//
// Proves that DiskBackedScrollback::truncate() uses O(1) cold-tier
// truncation via front_offset instead of O(N) decompress-and-rebuild.
// =========================================================================

/// Tier-aware truncate on DiskBackedScrollback avoids decompress-and-rebuild.
///
/// Pushes enough data to fill cold tier, then truncates. Verifies that
/// truncation preserves correct line content and uses front_offset
/// (line count changes without full rebuild).
#[test]
fn disk_backed_tier_aware_truncate_avoids_rebuild() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("tier-aware.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // Push 30 lines: 5 hot, 10 warm, 15 cold.
    for i in 0..30 {
        sb.push_str(&format!("L{i:03}")).unwrap();
    }
    let cold_before = sb.cold_line_count();
    let warm_before = sb.warm_line_count();
    let hot_before = sb.hot_line_count();
    assert!(cold_before > 0, "need cold data for this test");

    // Truncate to keep 22 lines (remove 8 from cold tier).
    sb.truncate(22).expect("truncate should succeed");
    assert_eq!(sb.line_count(), 22);

    // Hot and warm tiers should be unchanged (only cold was trimmed).
    assert_eq!(sb.hot_line_count(), hot_before, "hot tier unchanged");
    assert_eq!(sb.warm_line_count(), warm_before, "warm tier unchanged");
    assert_eq!(
        sb.cold_line_count(),
        cold_before - 8,
        "cold tier lost 8 lines"
    );

    // Verify line content: first kept line should be L008.
    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(first.to_string(), "L008");

    // Last line should be L029.
    let last = sb.get_line(21).unwrap().unwrap();
    assert_eq!(last.to_string(), "L029");

    // Verify all 22 lines are accessible and in order.
    for i in 0..22 {
        let line = sb.get_line(i).unwrap().unwrap();
        assert_eq!(line.to_string(), format!("L{:03}", 8 + i));
    }
}

/// Push with line_limit exercises constant-time truncation via front_offset.
///
/// With the old decompress-and-rebuild approach, every push to a full
/// scrollback was O(N). With tier-aware truncation, cold-tier removal
/// is O(1) per push.
#[test]
fn disk_backed_push_with_line_limit_constant_time() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("line-limit-perf.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(10)
        .with_warm_limit(20)
        .with_block_size(10);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();
    sb.set_line_limit(Some(100));

    // Fill to capacity.
    for i in 0..100 {
        sb.push_str(&format!("init-{i}")).unwrap();
    }
    assert_eq!(sb.line_count(), 100);

    // Push 200 more lines (each triggers a truncation).
    // With O(1) truncation this completes quickly. With O(N) rebuild
    // each push would decompress all 100 kept lines.
    let start = std::time::Instant::now();
    for i in 0..200 {
        sb.push_str(&format!("extra-{i}")).unwrap();
    }
    let elapsed = start.elapsed();

    assert_eq!(sb.line_count(), 100, "line limit enforced");

    // Verify last pushed line is accessible.
    let last = sb.get_line(99).unwrap().unwrap();
    assert_eq!(last.to_string(), "extra-199");

    // Sanity: this should complete in well under 5 seconds even on slow CI.
    // The old O(N) approach could take 10+ seconds with Zstd decompression.
    assert!(
        elapsed.as_secs() < 5,
        "push loop took {elapsed:?} — expected <5s with O(1) truncation"
    );
}

// =========================================================================
// Property tests: DiskBacked push_str + get_line round-trip (#2603)
// =========================================================================
//
// Kani cannot verify DiskBacked directly because the constructor requires
// file I/O, and the cold tier uses Zstd compression (FFI to C library).
// These property tests cover the full pipeline that Kani cannot reach:
// push_str → Line → serialize → compress → disk → decompress → deserialize
// → get_line → content comparison.

mod proptest_disk_backed {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating terminal-like line content.
    ///
    /// Includes plain ASCII, short strings, and Unicode to exercise
    /// different serialization paths (inline vs heap storage).
    fn line_content_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            // Plain ASCII (common case)
            "[a-zA-Z0-9 ]{0,120}",
            // Short strings (inline storage path)
            "[a-z]{0,10}",
            // Longer lines (heap storage path)
            "[a-zA-Z0-9 !@#$%]{120,300}",
            // Unicode content
            "(hello|世界|ñoño|café|🎉){1,5}",
        ]
    }

    proptest! {
        // 64 cases (not default 256): each case does full disk I/O through
        // hot→warm→cold tiers, making 256 iterations disproportionately
        // expensive (~12s). 64 cases still exercises all code paths.
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Push N lines via push_str, then verify every line via get_line.
        ///
        /// Exercises the full disk-backed pipeline: hot → warm → cold (disk)
        /// → read back with decompression and cache.
        #[test]
        fn push_str_get_line_roundtrip(
            lines in proptest::collection::vec(line_content_strategy(), 1..60),
        ) {
            let dir = aterm_tempfile::tempdir().unwrap();
            let path = dir.path().join("prop-roundtrip.dtrm");

            // Small limits to force tier transitions (hot→warm→cold)
            let config = DiskBackedScrollbackConfig::new(&path)
                .with_hot_limit(5)
                .with_warm_limit(10)
                .with_block_size(5);
            let mut sb = DiskBackedScrollback::with_config(config).unwrap();

            for line in &lines {
                sb.push_str(line).unwrap();
            }

            // Verify line count
            prop_assert_eq!(sb.line_count(), lines.len());

            // Verify tier sum invariant
            let hot = sb.hot_line_count();
            let warm = sb.warm_line_count();
            let cold = sb.cold_line_count();
            prop_assert_eq!(sb.line_count(), hot + warm + cold,
                "line_count ({}) != hot ({}) + warm ({}) + cold ({})",
                sb.line_count(), hot, warm, cold);

            // Verify every line round-trips correctly
            for (i, expected) in lines.iter().enumerate() {
                let retrieved = sb.get_line(i)
                    .unwrap_or_else(|e| panic!("get_line({i}) I/O error: {e}"))
                    .unwrap_or_else(|| panic!("get_line({i}) returned None"));
                let actual = retrieved.to_string();
                let tier = if i < cold { "cold" } else if i < cold + warm { "warm" } else { "hot" };
                prop_assert!(
                    actual == *expected,
                    "line {} content mismatch (tier: {}): got {:?}, expected {:?}",
                    i, tier, actual, expected
                );
            }

            // Out-of-bounds must return None
            let oob = sb.get_line(lines.len()).unwrap();
            prop_assert!(oob.is_none(), "out-of-bounds index must return None");
        }
    }
}
