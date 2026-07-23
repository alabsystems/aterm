// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `ScrollbackStorage` — extracted from `tests.rs` (#2100).

use super::*;

#[test]
fn scrollback_storage_default() {
    let storage = ScrollbackStorage::default();
    assert_eq!(storage.line_count(), 0);
    // Memory may have baseline overhead from the ring buffer allocation
}

#[test]
fn scrollback_storage_from_scrollback() {
    let sb = Scrollback::new(100, 1000, 10_000_000);
    let storage: ScrollbackStorage = sb.into();
    assert!(matches!(&storage, ScrollbackStorage::Memory(_)));
    assert_eq!(storage.line_count(), 0);
}

#[test]
fn scrollback_storage_memory_push_and_get() {
    let mut storage = ScrollbackStorage::default();

    storage.push_line(Line::from("Line 0")).unwrap();
    storage.push_line(Line::from("Line 1")).unwrap();
    storage.push_line(Line::from("Line 2")).unwrap();

    assert_eq!(storage.line_count(), 3);
    assert_eq!(
        storage
            .get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
    assert_eq!(
        storage
            .get_line(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 1"
    );
    assert_eq!(
        storage
            .get_line(2)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 2"
    );
    assert!(storage.get_line(3).expect("no error").is_none());
}

#[test]
fn scrollback_storage_memory_get_line_rev() {
    let mut storage = ScrollbackStorage::default();

    storage.push_line(Line::from("Oldest")).unwrap();
    storage.push_line(Line::from("Middle")).unwrap();
    storage.push_line(Line::from("Newest")).unwrap();

    assert_eq!(
        storage
            .get_line_rev(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Newest"
    );
    assert_eq!(
        storage
            .get_line_rev(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Middle"
    );
    assert_eq!(
        storage
            .get_line_rev(2)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Oldest"
    );
    assert!(storage.get_line_rev(3).expect("no error").is_none());
}

#[test]
fn scrollback_storage_memory_tier_metrics() {
    let sb = Scrollback::new(4, 8, 10_000_000);
    let mut storage: ScrollbackStorage = sb.into();

    storage.push_line(Line::from("Line 0")).unwrap();
    storage.push_line(Line::from("Line 1")).unwrap();

    let counts = storage.tier_line_counts();
    assert_eq!(counts.hot + counts.warm + counts.cold, storage.line_count());
    assert_eq!(storage.hot_limit(), 4);
    assert_eq!(storage.warm_limit(), 8);
    assert!(!storage.is_disk_backed());

    let cold = storage.cold_metrics();
    assert_eq!(cold.disk_used, None);
    assert_eq!(cold.memory_used, storage.cold_memory_used());
}

#[test]
fn scrollback_storage_memory_iter() {
    let mut storage = ScrollbackStorage::default();

    storage.push_line(Line::from("Line 0")).unwrap();
    storage.push_line(Line::from("Line 1")).unwrap();
    storage.push_line(Line::from("Line 2")).unwrap();

    let forward: Vec<_> = storage.iter().map(|line| line.to_string()).collect();
    assert_eq!(forward, vec!["Line 0", "Line 1", "Line 2"]);

    let reverse: Vec<_> = storage.iter_rev().map(|line| line.to_string()).collect();
    assert_eq!(reverse, vec!["Line 2", "Line 1", "Line 0"]);
}

#[test]
fn scrollback_storage_memory_clear() {
    let mut storage = ScrollbackStorage::default();

    storage.push_line(Line::from("Line")).unwrap();
    assert_eq!(storage.line_count(), 1);

    storage.clear().unwrap();
    assert_eq!(storage.line_count(), 0);
}

#[test]
fn scrollback_storage_memory_budget() {
    let mut storage = ScrollbackStorage::default();

    let initial_budget = storage.memory_budget();
    assert!(initial_budget > 0);

    storage
        .set_memory_budget(50_000_000)
        .expect("memory budget update should succeed");
    assert_eq!(storage.memory_budget(), 50_000_000);
}

#[test]
fn scrollback_storage_memory_line_limit() {
    let mut storage = ScrollbackStorage::default();

    // #7929: default is now a bounded cap, not `None`.
    assert_eq!(storage.line_limit(), Some(DEFAULT_LINE_LIMIT));

    storage.set_line_limit(Some(1000));
    assert_eq!(storage.line_limit(), Some(1000));

    storage.set_line_limit(None);
    assert!(storage.line_limit().is_none());
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_push_and_get() {
    // Use tempdir to get a directory, then specify a non-existent path inside
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();
    assert!(matches!(&storage, ScrollbackStorage::Disk(_)));

    storage.push_line(Line::from("Disk Line 0")).unwrap();
    storage.push_line(Line::from("Disk Line 1")).unwrap();

    assert_eq!(storage.line_count(), 2);
    assert_eq!(
        storage
            .get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Disk Line 0"
    );
    assert_eq!(
        storage
            .get_line(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Disk Line 1"
    );
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_get_line_rev() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    storage.push_line(Line::from("First")).unwrap();
    storage.push_line(Line::from("Last")).unwrap();

    assert_eq!(
        storage
            .get_line_rev(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Last"
    );
    assert_eq!(
        storage
            .get_line_rev(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "First"
    );
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_tier_metrics() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(3)
        .with_warm_limit(6);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    storage.push_line(Line::from("Line 0")).unwrap();
    storage.push_line(Line::from("Line 1")).unwrap();

    let counts = storage.tier_line_counts();
    assert_eq!(counts.hot + counts.warm + counts.cold, storage.line_count());
    assert_eq!(storage.hot_limit(), 3);
    assert_eq!(storage.warm_limit(), 6);
    assert!(storage.is_disk_backed());

    let cold = storage.cold_metrics();
    let disk_used = cold
        .disk_used
        .expect("disk-backed storage should report disk usage");
    assert_eq!(
        Some(disk_used),
        storage.cold_disk_used(),
        "cold_metrics disk_used should match direct cold_disk_used()"
    );
    assert_eq!(cold.memory_used, storage.cold_memory_used());
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_iter() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    storage.push_line(Line::from("First")).unwrap();
    storage.push_line(Line::from("Second")).unwrap();
    storage.push_line(Line::from("Third")).unwrap();

    let forward: Vec<_> = storage.iter().map(|line| line.to_string()).collect();
    assert_eq!(forward, vec!["First", "Second", "Third"]);

    let reverse: Vec<_> = storage.iter_rev().map(|line| line.to_string()).collect();
    assert_eq!(reverse, vec!["Third", "Second", "First"]);
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_clear() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    storage.push_line(Line::from("To clear")).unwrap();
    assert_eq!(storage.line_count(), 1);

    storage.clear().unwrap();
    assert_eq!(storage.line_count(), 0);
}

#[cfg(feature = "disk-tier")]
#[test]
fn scrollback_storage_disk_memory_budget() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&path).with_memory_budget(25_000_000);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    assert_eq!(storage.memory_budget(), 25_000_000);

    storage
        .set_memory_budget(50_000_000)
        .expect("memory budget update should succeed");
    assert_eq!(storage.memory_budget(), 50_000_000);
}

// --- checkpoint_snapshot_fast tests (#5946) ---

#[test]
fn checkpoint_snapshot_fast_all_hot_preserves_lines() {
    // When all lines are in hot tier, fast snapshot captures everything.
    let sb = Scrollback::new(100, 1000, 10_000_000);
    let mut storage: ScrollbackStorage = sb.into();

    for i in 0..5 {
        storage
            .push_line(Line::from(format!("Hot {i}").as_str()))
            .unwrap();
    }

    let snapshot = storage.checkpoint_snapshot_fast();
    assert_eq!(snapshot.line_count(), 5);
    for i in 0..5 {
        assert_eq!(
            snapshot.get_line(i).unwrap().unwrap().to_string(),
            format!("Hot {i}")
        );
    }
}

#[test]
fn checkpoint_snapshot_fast_skips_cold_tier_lines() {
    // Push enough lines to fill hot → warm → cold, then verify the fast
    // snapshot contains only hot + warm lines (cold lines excluded).
    //
    // Tier config: hot=4, warm=8, block_size=4, budget=10MB
    // Push 20 lines: cold gets ≥8 lines, warm gets up to 8, hot gets remainder.
    let sb = Scrollback::with_block_size(4, 8, 10_000_000, 4);
    let mut storage: ScrollbackStorage = sb.into();

    for i in 0..20 {
        storage
            .push_line(Line::from(format!("Line {i}").as_str()))
            .unwrap();
    }

    let counts = storage.tier_line_counts();
    assert!(counts.cold > 0, "cold tier should have lines");
    assert_eq!(
        counts.hot + counts.warm + counts.cold,
        20,
        "all 20 lines accounted for across tiers"
    );

    let full_snapshot = storage.checkpoint_snapshot();
    let fast_snapshot = storage.checkpoint_snapshot_fast();

    // Full snapshot captures everything.
    assert_eq!(full_snapshot.line_count(), 20);

    // Fast snapshot skips cold tier.
    let expected_fast_lines = counts.hot + counts.warm;
    assert_eq!(fast_snapshot.line_count(), expected_fast_lines);

    // The fast snapshot should contain the MOST RECENT lines (hot + warm),
    // which are the lines at the end of the original storage.
    let cold_count = counts.cold;
    for i in 0..expected_fast_lines {
        let original_idx = cold_count + i;
        let original = storage.get_line(original_idx).unwrap().unwrap().to_string();
        let snapshotted = fast_snapshot.get_line(i).unwrap().unwrap().to_string();
        assert_eq!(
            original, snapshotted,
            "fast snapshot line {i} should match original line {original_idx}"
        );
    }
}

#[test]
fn checkpoint_snapshot_fast_empty_scrollback() {
    let storage = ScrollbackStorage::default();
    let snapshot = storage.checkpoint_snapshot_fast();
    assert_eq!(snapshot.line_count(), 0);
}

#[test]
fn checkpoint_snapshot_fast_preserves_line_limit() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.set_line_limit(Some(50));
    let mut storage: ScrollbackStorage = sb.into();

    for i in 0..10 {
        storage
            .push_line(Line::from(format!("Line {i}").as_str()))
            .unwrap();
    }

    let snapshot = storage.checkpoint_snapshot_fast();
    assert_eq!(snapshot.line_limit(), Some(50));
    assert_eq!(snapshot.line_count(), 10);
}

#[cfg(feature = "disk-tier")]
#[test]
fn checkpoint_snapshot_fast_disk_backed_skips_cold() {
    let temp_dir = aterm_tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("scrollback.dtrm");
    // Small tiers to force cold tier population quickly.
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(4)
        .with_warm_limit(8)
        .with_block_size(4);
    let disk_sb =
        DiskBackedScrollback::with_config(config).expect("Failed to create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();

    for i in 0..20 {
        storage
            .push_line(Line::from(format!("Disk {i}").as_str()))
            .unwrap();
    }

    let counts = storage.tier_line_counts();
    assert!(counts.cold > 0, "disk cold tier should have lines");

    let fast_snapshot = storage.checkpoint_snapshot_fast();
    let expected = counts.hot + counts.warm;
    assert_eq!(
        fast_snapshot.line_count(),
        expected,
        "fast snapshot should have hot+warm lines only"
    );

    // Verify the snapshot contains the most recent lines, not the oldest.
    let newest_original = storage.get_line_rev(0).unwrap().unwrap().to_string();
    let newest_snapshot = fast_snapshot.get_line_rev(0).unwrap().unwrap().to_string();
    assert_eq!(
        newest_original, newest_snapshot,
        "newest line should match between original and fast snapshot"
    );
}

// --- Warm block quarantine tests (#5947) ---

/// Corrupted warm block is quarantined (dropped) after QUARANTINE_THRESHOLD
/// consecutive failed eviction attempts, and its line count is subtracted.
///
/// Scenario: inject a corrupt warm block at the front of warm, then push
/// lines that trigger hot→warm promotion (which evicts the corrupt block).
/// After 3 promotions, the corrupt block should be quarantined and its lines
/// dropped from the total count.
#[test]
fn test_corrupt_warm_block_quarantined_after_threshold() {
    // hot=4, warm_limit=4, block_size=4, budget=10MB (generous).
    // Each hot→warm promotion triggers warm→cold eviction since warm holds >4 lines.
    let mut sb = Scrollback::with_block_size(4, 4, 10_000_000, 4);

    // Push 8 lines: 4 go to hot, then promoted to warm (1 block of 4), next 4 in hot.
    for i in 0..8 {
        sb.push_str(&format!("line-{i}"));
    }
    assert_eq!(sb.hot_line_count(), 4);
    assert_eq!(sb.warm_line_count(), 4);
    assert_eq!(sb.line_count(), 8);

    // Inject corrupt block (10 lines) at front of warm.
    let corrupt_lines = 10;
    sb.inject_corrupted_warm_block(corrupt_lines);
    let count_after_inject = sb.line_count();
    assert_eq!(count_after_inject, 18);

    // Push 3 batches of 4 lines. Each batch fills hot (4 lines), triggering
    // promote_hot_to_warm, which evicts the corrupt block from warm.
    // Eviction 1: decompress() fails (failures=1), push back, return false.
    // Eviction 2: decompress() fails (failures=2), push back, return false.
    // Eviction 3: decompress() fails (failures=3), quarantined! Block dropped, line_count -= 10.
    for batch in 0..3 {
        for i in 0..4 {
            sb.push_str(&format!("batch{batch}-{i}"));
        }
    }

    // Expected: 18 (after inject) + 12 (pushed) - 10 (quarantined) = 20
    let expected = count_after_inject + 12 - corrupt_lines;
    assert_eq!(
        sb.line_count(),
        expected,
        "corrupt block should be quarantined: 10 lines dropped from total"
    );

    // Verify remaining lines are accessible (no data loss for healthy lines).
    let all_lines: Vec<_> = sb.iter().map(|l| l.to_string()).collect();
    assert_eq!(
        all_lines.len(),
        sb.line_count(),
        "iterator should yield exactly line_count() lines after quarantine"
    );
}

/// After quarantine, iteration yields only non-corrupted lines and completes
/// without hanging or redundant decompression attempts.
#[test]
fn test_iteration_complete_after_quarantine() {
    let mut sb = Scrollback::with_block_size(4, 4, 10_000_000, 4);

    // Push lines and inject corruption.
    for i in 0..8 {
        sb.push_str(&format!("line-{i}"));
    }
    sb.inject_corrupted_warm_block(5);

    // Drive quarantine: 3 batches of 4 lines.
    for batch in 0..3 {
        for i in 0..4 {
            sb.push_str(&format!("b{batch}-{i}"));
        }
    }

    // Collect all lines via forward iterator.
    let forward: Vec<_> = sb.iter().map(|l| l.to_string()).collect();
    assert_eq!(
        forward.len(),
        sb.line_count(),
        "forward iteration should yield line_count() lines"
    );

    // Collect all lines via reverse iterator.
    let reverse: Vec<_> = sb.iter_rev().map(|l| l.to_string()).collect();
    assert_eq!(
        reverse.len(),
        sb.line_count(),
        "reverse iteration should yield line_count() lines"
    );

    // Forward and reverse should contain the same lines (in opposite order).
    let mut rev_sorted = reverse.clone();
    rev_sorted.reverse();
    assert_eq!(
        forward, rev_sorted,
        "forward and reversed-reverse should match"
    );
}

/// Line limit enforcement recovers after a corrupt warm block is quarantined.
///
/// When a corrupt block sits at the front of warm, truncation may fail because
/// it needs to decompress the boundary block. After the block is quarantined
/// (dropped via eviction), the line limit is re-enforced on subsequent pushes.
#[test]
fn test_line_limit_enforced_after_quarantine() {
    // hot=4, warm_limit=4, block_size=4, budget=10MB.
    let mut sb = Scrollback::with_block_size(4, 4, 10_000_000, 4);

    // Push initial lines.
    for i in 0..8 {
        sb.push_str(&format!("line-{i}"));
    }

    // Set a line limit.
    let limit = 25;
    sb.set_line_limit(Some(limit));

    // Inject corrupt block (10 lines) at front of warm.
    sb.inject_corrupted_warm_block(10);
    // line_count = 18 (8 original + 10 corrupt)

    // Drive quarantine: push 3 batches of 4 to trigger 3 eviction attempts.
    for batch in 0..3 {
        for i in 0..4 {
            sb.push_str(&format!("b{batch}-{i}"));
        }
    }
    // After quarantine: 18 + 12 - 10 = 20, which is under the limit of 25.

    // Now push enough lines to exceed the limit. Without quarantine, the corrupt
    // block would block truncation and the limit would be unenforced.
    for i in 0..20 {
        sb.push_str(&format!("extra-{i}"));
    }

    // After quarantine, the line limit should be enforced.
    assert!(
        sb.line_count() <= limit,
        "line limit should be enforced after quarantine: count={}, limit={limit}",
        sb.line_count()
    );
}

/// Regression (#5950): hot-tier lines are returned as Cow::Borrowed (zero-copy),
/// warm-tier lines as Cow::Owned (decompressed). Verifies the Cow variant
/// matches the expected tier for each access.
#[test]
fn get_line_returns_borrowed_for_hot_tier() {
    use std::borrow::Cow;

    // Small limits to force tier promotion: block_size=2, hot_limit=4, warm_limit=10.
    let mut sb = Scrollback::with_block_size(4, 10, 10_000_000, 2);

    // Push 6 lines: 2 will be promoted to warm (1 block), 4 remain in hot.
    for i in 0..6 {
        sb.push_str(&format!("line-{i}"));
    }
    assert_eq!(sb.line_count(), 6);
    assert_eq!(sb.hot_line_count(), 4, "4 lines in hot tier");
    assert_eq!(sb.warm_line_count(), 2, "2 lines promoted to warm");

    // Lines 0-1 are in warm tier → Cow::Owned (decompressed from LZ4).
    for idx in 0..2 {
        let cow = sb.get_line(idx).unwrap().expect("warm line present");
        assert!(
            matches!(cow, Cow::Owned(_)),
            "warm-tier line {idx} should be Cow::Owned, got Borrowed"
        );
        assert_eq!(cow.to_string(), format!("line-{idx}"));
    }

    // Lines 2-5 are in hot tier → Cow::Borrowed (zero-copy).
    for idx in 2..6 {
        let cow = sb.get_line(idx).unwrap().expect("hot line present");
        assert!(
            matches!(cow, Cow::Borrowed(_)),
            "hot-tier line {idx} should be Cow::Borrowed, got Owned"
        );
        assert_eq!(cow.to_string(), format!("line-{idx}"));
    }

    // Reverse access: rev_idx 0 = most recent = hot tier → Borrowed.
    let newest = sb.get_line_rev(0).unwrap().expect("newest line present");
    assert!(
        matches!(newest, Cow::Borrowed(_)),
        "newest line (hot tier) should be Cow::Borrowed via get_line_rev"
    );
    assert_eq!(newest.to_string(), "line-5");
}
