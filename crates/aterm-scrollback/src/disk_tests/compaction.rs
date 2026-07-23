// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compaction + orphan temp file cleanup tests for `DiskColdTier` (#5916).

use super::*;

/// Push 5 pages, truncate front 3, force_compact, verify file shrinks and
/// surviving pages are readable.
#[test]
fn test_compact_after_front_truncation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..5 {
        let (compressed, lc) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }
    let file_size_before = std::fs::metadata(&path).unwrap().len();
    assert_eq!(cold.line_count(), 50);

    // Truncate 3 pages from front (below auto-compact threshold).
    cold.truncate_front_lines(30);
    assert_eq!(cold.line_count(), 20);
    assert_eq!(cold.page_count(), 2);

    // File unchanged yet (below 64 KB threshold).
    assert_eq!(std::fs::metadata(&path).unwrap().len(), file_size_before);

    // Force compaction.
    cold.force_compact().unwrap();

    let file_size_after = std::fs::metadata(&path).unwrap().len();
    assert!(
        file_size_after < file_size_before,
        "compaction should shrink file: before={file_size_before}, after={file_size_after}"
    );

    // Remaining 2 pages should be readable.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page3-Line0"
    );
    assert_eq!(
        cold.get_line(19).unwrap().unwrap().to_string(),
        "Page4-Line9"
    );
    assert!(cold.get_line(20).unwrap().is_none());

    // Header should reflect compacted state.
    let (pc, lc) = read_header_counts(&path);
    assert_eq!(pc, 2);
    assert_eq!(lc, 20);
}

/// Push 2 small pages, truncate front 1 — below COMPACT_MIN_DEAD_BYTES,
/// so auto-compaction should NOT fire.
#[test]
fn test_compact_skipped_below_threshold() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c1, l1) = create_test_page(10, "Page0");
    cold.push_compressed(&c1, l1).unwrap();
    let (c2, l2) = create_test_page(10, "Page1");
    cold.push_compressed(&c2, l2).unwrap();

    let file_size_before = std::fs::metadata(&path).unwrap().len();

    // Truncate first page.
    cold.truncate_front_lines(10);
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 1);

    // File should NOT shrink (dead space < 64 KB).
    let file_size_after = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        file_size_before, file_size_after,
        "small dead space should not trigger compaction"
    );

    // Data still readable.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page1-Line0"
    );
}

/// After compaction, verify the temp file is cleaned up.
#[test]
fn test_compact_cleans_up_temp_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    let tmp_path = path.with_extension("dtrm.compact");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..5 {
        let (compressed, lc) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }

    cold.truncate_front_lines(30);
    cold.force_compact().unwrap();

    // Atomic rename replaces original with temp — no leftover temp file.
    assert!(
        !tmp_path.exists(),
        "temp file should not remain after compaction"
    );
    assert!(
        path.exists(),
        "original path should still exist after rename"
    );
}

/// Non-page-aligned truncation leaves front_offset > 0. Compaction must trim
/// the consumed prefix from the first page so that a reload does not resurrect
/// the consumed lines.
#[test]
fn test_compact_trims_front_offset_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Push 5 pages × 10 lines = 50 lines.
    for page_num in 0..5 {
        let (compressed, lc) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }
    assert_eq!(cold.line_count(), 50);

    // Truncate 13 lines: drops page 0 (10 lines) + sets front_offset=3 on page 1.
    cold.truncate_front_lines(13);
    assert_eq!(cold.line_count(), 37);
    assert_eq!(cold.page_count(), 4);

    // Force compaction while front_offset > 0.
    cold.force_compact().unwrap();
    assert_eq!(
        cold.line_count(),
        37,
        "line count unchanged after compaction"
    );

    // Verify the oldest surviving line is correct (Page1-Line3, not Page1-Line0).
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page1-Line3",
        "first line should be Page1-Line3 after trimming 13 lines"
    );
    assert_eq!(
        cold.get_line(36).unwrap().unwrap().to_string(),
        "Page4-Line9"
    );
    assert!(cold.get_line(37).unwrap().is_none());

    // Header should reflect the logical (trimmed) count.
    let (pc, lc) = read_header_counts(&path);
    assert_eq!(pc, 4);
    assert_eq!(lc, 37);

    // Reload from disk — the consumed lines must NOT reappear.
    drop(cold);
    let config = DiskColdConfig::new(&path);
    let reloaded = DiskColdTier::with_config(config).unwrap();
    assert_eq!(reloaded.line_count(), 37, "reloaded line count matches");
    assert_eq!(
        reloaded.get_line(0).unwrap().unwrap().to_string(),
        "Page1-Line3",
        "consumed lines must not reappear after reload"
    );
    assert_eq!(
        reloaded.get_line(36).unwrap().unwrap().to_string(),
        "Page4-Line9"
    );
}

/// Compact, then push more pages — verify they append at the correct offset
/// and the file reloads cleanly.
#[test]
fn test_push_after_compact() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..5 {
        let (compressed, lc) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }

    // Truncate + compact.
    cold.truncate_front_lines(30);
    cold.force_compact().unwrap();
    let file_size_after_compact = std::fs::metadata(&path).unwrap().len();

    // Push 3 more pages.
    for page_num in 5..8 {
        let (compressed, lc) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }

    assert_eq!(cold.line_count(), 50);
    assert_eq!(cold.page_count(), 5);

    let file_size_after_push = std::fs::metadata(&path).unwrap().len();
    assert!(
        file_size_after_push > file_size_after_compact,
        "file should grow after pushing new pages"
    );

    // Verify data from both surviving and new pages.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page3-Line0"
    );
    assert_eq!(
        cold.get_line(20).unwrap().unwrap().to_string(),
        "Page5-Line0"
    );
    assert_eq!(
        cold.get_line(49).unwrap().unwrap().to_string(),
        "Page7-Line9"
    );

    // Reload from disk — verify file is self-consistent.
    drop(cold);
    let config = DiskColdConfig::new(&path);
    let reloaded = DiskColdTier::with_config(config).unwrap();
    assert_eq!(reloaded.line_count(), 50);
    assert_eq!(reloaded.page_count(), 5);
    assert_eq!(
        reloaded.get_line(0).unwrap().unwrap().to_string(),
        "Page3-Line0"
    );
    assert_eq!(
        reloaded.get_line(49).unwrap().unwrap().to_string(),
        "Page7-Line9"
    );
}
