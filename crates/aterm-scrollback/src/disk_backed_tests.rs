// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for DiskBackedScrollback.

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

    let initial_cold_mem = sb.cold_memory_used();

    for i in 0..25 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    assert!(sb.cold_line_count() > 0);
    assert!(sb.cold_disk_used() > 0);
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

    let initial_total = sb.total_memory_used();

    for i in 0..4 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }
    let hot_warm = sb.memory_used();
    let cold_mem = sb.cold_memory_used();
    assert_eq!(sb.total_memory_used(), hot_warm + cold_mem);

    for i in 4..25 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    // Disk data is not counted.
    let hot_warm = sb.memory_used();
    let cold_mem = sb.cold_memory_used();
    assert_eq!(sb.total_memory_used(), hot_warm + cold_mem);
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

// Tier-aware remove_newest.

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

/// `remove_newest(1)` on a large scrollback costs about one block of memory,
/// not the whole history.
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

    // Enough lines for all three tiers (hot=5, warm=200, cold ~800).
    let total = 1_000;
    for i in 0..total {
        sb.push_str(&format!("DiskMem-{i}")).unwrap();
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");
    assert!(sb.warm_line_count() > 0, "need warm tier data");

    // Drain hot so the removal reaches the compressed tiers.
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

// Line limit.

#[test]
fn disk_backed_scrollback_line_limit_default_is_safe_cap() {
    // A bounded default keeps runaway output from filling the disk.
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

    for i in 0..15 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    assert_eq!(sb.line_count(), 10);

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

    for i in 0..20 {
        sb.push_str(&format!("Line {i}")).unwrap();
    }
    assert_eq!(sb.line_count(), 20);

    sb.set_line_limit(Some(10));
    assert_eq!(sb.line_count(), 10);

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

    sb.set_line_limit(Some(0));
    assert_eq!(sb.line_count(), 0);
}

/// After warm→cold eviction, `line_count` equals the tier sum and every line is
/// retrievable.
#[test]
fn disk_backed_scrollback_evict_warm_cold_no_data_loss() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Small limits force warm→cold eviction.
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    let total_lines = 25;
    for i in 0..total_lines {
        sb.push_str(&format!("Line {i}")).unwrap();
    }

    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    let cold = sb.cold_line_count();
    assert_eq!(
        sb.line_count(),
        hot + warm + cold,
        "line_count ({}) != hot ({hot}) + warm ({warm}) + cold ({cold})",
        sb.line_count()
    );

    for i in 0..sb.line_count() {
        let line = sb
            .get_line(i)
            .expect("no error")
            .expect("line should remain retrievable after warm/cold eviction");
        assert_eq!(line.to_string(), format!("Line {i}"));
    }

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

/// `truncate` trims the cold tier through `front_offset` and leaves hot and
/// warm untouched.
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

    // Keeping 22 removes 8 lines, all from the cold tier.
    sb.truncate(22).expect("truncate should succeed");
    assert_eq!(sb.line_count(), 22);

    assert_eq!(sb.hot_line_count(), hot_before, "hot tier unchanged");
    assert_eq!(sb.warm_line_count(), warm_before, "warm tier unchanged");
    assert_eq!(
        sb.cold_line_count(),
        cold_before - 8,
        "cold tier lost 8 lines"
    );

    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(first.to_string(), "L008");

    let last = sb.get_line(21).unwrap().unwrap();
    assert_eq!(last.to_string(), "L029");

    for i in 0..22 {
        let line = sb.get_line(i).unwrap().unwrap();
        assert_eq!(line.to_string(), format!("L{:03}", 8 + i));
    }
}

/// Pushing past the line limit truncates the front and keeps the newest lines
/// readable.
#[test]
fn disk_backed_push_with_line_limit_keeps_the_newest_lines() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("line-limit-perf.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(10)
        .with_warm_limit(20)
        .with_block_size(10);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();
    sb.set_line_limit(Some(100));

    for i in 0..100 {
        sb.push_str(&format!("init-{i}")).unwrap();
    }
    assert_eq!(sb.line_count(), 100);

    // Each push past the limit truncates.
    for i in 0..200 {
        sb.push_str(&format!("extra-{i}")).unwrap();
    }

    assert_eq!(sb.line_count(), 100, "line limit enforced");

    let last = sb.get_line(99).unwrap().unwrap();
    assert_eq!(last.to_string(), "extra-199");
}

// push_str → serialize → compress → disk → decompress → get_line round-trip.

/// Terminal-like line content across the serialization paths: plain ASCII,
/// short (inline storage), long (heap storage) and multi-byte Unicode.
fn generated_line(next: &mut impl FnMut(u64) -> u64) -> String {
    const ASCII: &[u8] = b"abcXYZ019 ";
    const WIDE: &[u8] = b"abcXYZ019 !@#$%";
    const WORDS: &[&str] = &["hello", "世界", "ñoño", "café", "🎉"];
    let pick = |next: &mut dyn FnMut(u64) -> u64, from: &[u8], n: u64| -> String {
        (0..n)
            .map(|_| char::from(from[next(from.len() as u64) as usize]))
            .collect()
    };
    match next(4) {
        0 => {
            let n = next(121);
            pick(next, ASCII, n)
        }
        1 => {
            let n = next(11);
            (0..n).map(|_| char::from(b'a' + next(26) as u8)).collect()
        }
        2 => {
            let n = 120 + next(181);
            pick(next, WIDE, n)
        }
        _ => (0..1 + next(5))
            .map(|_| WORDS[next(WORDS.len() as u64) as usize])
            .collect(),
    }
}

/// Push N generated lines, then check every line, the tier sum and the
/// out-of-bounds read; small limits push each case through hot→warm→cold.
#[test]
fn push_str_get_line_roundtrip() {
    let mut rng = aterm_spec::rng::XorShift::new(0xD15C_BAC0);
    let mut next = move |n: u64| rng.below(n);
    for _ in 0..64 {
        let lines: Vec<String> = (0..1 + next(59))
            .map(|_| generated_line(&mut next))
            .collect();
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.dtrm");

        let config = DiskBackedScrollbackConfig::new(&path)
            .with_hot_limit(5)
            .with_warm_limit(10)
            .with_block_size(5);
        let mut sb = DiskBackedScrollback::with_config(config).unwrap();
        for line in &lines {
            sb.push_str(line).unwrap();
        }

        assert_eq!(sb.line_count(), lines.len());
        let (hot, warm, cold) = (
            sb.hot_line_count(),
            sb.warm_line_count(),
            sb.cold_line_count(),
        );
        assert_eq!(sb.line_count(), hot + warm + cold, "tier sum");

        for (i, expected) in lines.iter().enumerate() {
            let actual = sb
                .get_line(i)
                .unwrap_or_else(|e| panic!("get_line({i}) I/O error: {e}"))
                .unwrap_or_else(|| panic!("get_line({i}) returned None"))
                .to_string();
            let tier = if i < cold {
                "cold"
            } else if i < cold + warm {
                "warm"
            } else {
                "hot"
            };
            assert_eq!(actual, *expected, "line {i} ({tier})");
        }

        assert!(
            sb.get_line(lines.len()).unwrap().is_none(),
            "out-of-bounds index must return None"
        );
    }
}

// Eager promotion under memory pressure.

/// Push `n` lines just under 200 bytes, reporting whether the watermark was
/// ever seen at Yellow or above.
///
/// Sampled per push, not at the end: promoting a block drops `budgeted_bytes`
/// below the Yellow-exit threshold, so a run that promoted throughout reads
/// Green afterwards.
fn pressure_lines(sb: &mut DiskBackedScrollback, n: usize) -> bool {
    let mut saw_yellow = false;
    for i in 0..n {
        sb.push_str(&format!("{i:04}{}", "x".repeat(196)))
            .expect("push into disk-backed scrollback");
        saw_yellow |= sb.watermark_level() >= WatermarkLevel::Yellow;
    }
    saw_yellow
}

/// A disk-backed scrollback whose ordinary promotion boundary is out of reach.
fn pressure_fixture(path: &std::path::Path, budget: usize) -> DiskBackedScrollback {
    let config = DiskBackedScrollbackConfig::new(path)
        .with_hot_limit(HOT_LIMIT)
        .with_warm_limit(2000)
        .with_block_size(10)
        .with_memory_budget(budget);
    DiskBackedScrollback::with_config(config).expect("create disk scrollback")
}

/// Lines each arm pushes.
const LINES: usize = 150;

/// Above [`LINES`], so neither arm reaches the ordinary promotion boundary
/// (`hot.len() >= hot_limit`): anything outside hot means the eager path fired.
const HOT_LIMIT: usize = 200;

const _: () = assert!(HOT_LIMIT > LINES);

/// The disk-backed twin of `watermark_eager_promotion_fires_under_pressure`:
/// under Yellow, `push_line` promotes at `hot.len() >= block_size`, a block
/// before the ordinary `hot_limit` boundary.
///
/// The control arm is the point: the same [`LINES`] under a budget they cannot
/// stress must stay wholly in hot, or a pass could equally come from eviction,
/// from a block_size promotion that fires at every level, or from a fixture
/// that never reached Yellow.
#[test]
fn disk_backed_eager_promotion_fires_under_pressure() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();

    // Control: a budget those lines cannot stress. Nothing leaves hot.
    let mut calm = pressure_fixture(&dir.path().join("calm.dtrm"), 100_000_000);
    assert!(
        !pressure_lines(&mut calm, LINES),
        "control fixture must never reach Yellow; budgeted={} budget={}",
        calm.budgeted_bytes(),
        calm.memory_budget(),
    );
    assert_eq!(
        calm.hot_line_count(),
        LINES,
        "with no pressure and hot_limit={HOT_LIMIT} unreached, every line stays \
         hot; warm={} cold={}",
        calm.warm_line_count(),
        calm.cold_line_count(),
    );

    // The same push under a budget those lines do stress.
    let mut tight = pressure_fixture(&dir.path().join("tight.dtrm"), 20_000);
    assert!(
        pressure_lines(&mut tight, LINES),
        "fixture must actually reach the Yellow watermark; final level={:?} \
         budgeted={} budget={}",
        tight.watermark_level(),
        tight.budgeted_bytes(),
        tight.memory_budget(),
    );
    assert!(
        tight.hot_line_count() < HOT_LIMIT,
        "the ordinary hot_limit boundary must stay unreached, or this test \
         cannot tell eager promotion from it; hot={}",
        tight.hot_line_count(),
    );
    assert!(
        tight.warm_line_count() > 0 || tight.cold_line_count() > 0,
        "eager promotion should move data out of hot under Yellow pressure; \
         hot={} warm={} cold={}",
        tight.hot_line_count(),
        tight.warm_line_count(),
        tight.cold_line_count(),
    );
}
