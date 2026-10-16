// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Compaction tests for `DiskColdTier`.

use super::*;
use aterm_tempfile::tempdir;

/// Compaction fires once dead space exceeds live data and shrinks the file.
#[test]
fn disk_cold_compaction_triggers_on_truncate() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-trigger.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..10 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }

    let file_size_before = std::fs::metadata(&path).unwrap().len();
    assert_eq!(cold.line_count(), 50);
    assert_eq!(cold.page_count(), 10);

    // Truncate 6 of 10 pages: 60% dead fires compaction.
    cold.truncate_front_lines(30);
    assert_eq!(cold.line_count(), 20);
    assert_eq!(cold.page_count(), 4);

    let file_size_after = std::fs::metadata(&path).unwrap().len();
    assert!(
        file_size_after < file_size_before,
        "file should shrink after compaction: before={file_size_before}, after={file_size_after}"
    );

    let (header_pages, header_lines) = read_header_counts(&path);
    assert_eq!(header_pages, 4);
    assert_eq!(header_lines, 20);
}

/// Data survives compaction: all remaining lines are readable and correct.
#[test]
fn disk_cold_compaction_preserves_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-data.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..10 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }

    // Truncate 35 lines (7 pages). Remaining: P7, P8, P9 (15 lines).
    cold.truncate_front_lines(35);
    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.page_count(), 3);

    for page in 7..10 {
        for line in 0..5 {
            let idx = (page - 7) * 5 + line;
            let expected = format!("P{page}-Line{line}");
            let actual = cold.get_line(idx).unwrap().unwrap().to_string();
            assert_eq!(actual, expected, "line {idx} mismatch");
        }
    }

    // Reload from disk and verify data survives.
    drop(cold);
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.page_count(), 3);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P7-Line0");
    assert_eq!(cold.get_line(14).unwrap().unwrap().to_string(), "P9-Line4");
}

/// Compaction is idempotent: calling compact() twice yields the same file size.
#[test]
fn disk_cold_compaction_idempotent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-idem.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..10 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    cold.truncate_front_lines(35); // triggers compaction

    let size_after_first = std::fs::metadata(&path).unwrap().len();

    // Manually call compact() again — should be a no-op since dead_bytes == 0.
    cold.compact().unwrap();

    let size_after_second = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        size_after_first, size_after_second,
        "second compaction should not change file size"
    );

    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P7-Line0");
}

/// Compaction with front_offset > 0 trims the consumed prefix from the first page:
/// front_offset is not persisted, so untrimmed lines would reappear after reload.
#[test]
fn disk_cold_compaction_trims_front_offset() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-trim.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..10 {
        let (c, n) = create_test_page(10, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    assert_eq!(cold.line_count(), 100);

    // Drops 6 full pages and leaves front_offset = 3 on page 6; 60% dead fires
    // compaction.
    cold.truncate_front_lines(63);
    assert_eq!(cold.line_count(), 37);

    // Verify data before reload — first surviving line is P6-Line3 (skipped 3).
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P6-Line3",);
    assert_eq!(cold.get_line(36).unwrap().unwrap().to_string(), "P9-Line9",);

    // Compaction physically trimmed the consumed prefix, so the header counts match.
    let (header_pages, header_lines) = read_header_counts(&path);
    assert_eq!(header_pages, 4, "4 surviving pages after compaction");
    assert_eq!(
        header_lines, 37,
        "header line count must equal logical (37), not physical (40)"
    );

    drop(cold);
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(cold.line_count(), 37, "line count after reload");
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P6-Line3");
    assert_eq!(cold.get_line(36).unwrap().unwrap().to_string(), "P9-Line9");
}

/// A crash mid-compaction leaves an orphan `.dtrm.tmp`; the next `with_config()`
/// load removes it.
#[test]
fn disk_cold_load_cleans_orphan_compact_temp_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orphan-clean.dtrm");
    let tmp_path = path.with_extension("dtrm.tmp");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();
    for i in 0..3 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    drop(cold);

    // Simulate a crash mid-compaction: leave an orphan .dtrm.tmp file.
    std::fs::write(&tmp_path, b"incomplete compaction data").unwrap();
    assert!(
        tmp_path.exists(),
        "orphan temp file should exist before load"
    );

    // Reload — with_config must clean up the orphan before loading.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert!(
        !tmp_path.exists(),
        "orphan temp file must be removed after load"
    );

    // Original data is intact.
    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P0-Line0");
    assert_eq!(cold.get_line(14).unwrap().unwrap().to_string(), "P2-Line4");
}

/// A failed compaction re-maps the original file so cold reads keep working.
#[test]
fn failed_compaction_keeps_cold_reads_working() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-fail.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..10 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    assert_eq!(cold.line_count(), 50);

    // File::create fails on a directory, so compact() errors right after
    // flush_and_drop_mmap().
    let tmp_path = path.with_extension("dtrm.tmp");
    std::fs::create_dir(&tmp_path).unwrap();

    // dead (6 pages) > live (4): compaction fires and fails; truncate_front_lines
    // swallows the error.
    cold.truncate_front_lines(30);
    assert_eq!(cold.line_count(), 20);
    assert_eq!(cold.page_count(), 4);

    // The mmap was re-established: surviving pages, still at their original
    // offsets in the untouched file, stay readable.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "P6-Line0",
        "first surviving line must remain readable after a failed compaction"
    );
    assert_eq!(
        cold.get_line(19).unwrap().unwrap().to_string(),
        "P9-Line4",
        "last surviving line must remain readable after a failed compaction"
    );
}
