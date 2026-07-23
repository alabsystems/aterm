// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Basic operations, clear, and mmap lifecycle tests for `DiskColdTier`.

use super::*;

fn create_large_test_page(line_bytes: usize, prefix: &str) -> (Vec<u8>, usize) {
    let line = Line::from(format!("{prefix}-{}", "x".repeat(line_bytes)).as_str());
    let serialized = serialize_lines(&[line]);
    let compressed = zstd::encode_all(serialized.as_slice(), 3).unwrap();
    (compressed, 1)
}

/// Regression: #1004 - test failed when get_line() assertions were added.
/// In-memory mode is metadata-only by design; data is not stored.
#[test]
fn disk_cold_in_memory() {
    let mut cold = DiskColdTier::new();
    assert!(cold.is_empty());
    assert!(!cold.is_disk_backed());

    // Push first page
    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    // In-memory mode is metadata-only - data not stored, so get_line() won't work
    // Only verify metadata tracking
    assert!(!cold.is_empty(), "should not be empty after push");
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 1);
    let err = cold
        .get_line(0)
        .expect_err("metadata-only in-memory mode must surface read failure");
    assert!(
        matches!(err, ScrollbackError::Io(_)),
        "expected I/O error, got: {err:?}"
    );

    // Push second page - verify metadata accumulates correctly
    let (compressed2, line_count2) = create_test_page(15, "Page1");
    cold.push_compressed(&compressed2, line_count2).unwrap();
    assert_eq!(cold.line_count(), 25, "line count should accumulate");
    assert_eq!(cold.page_count(), 2, "page count should increment");
}

#[test]
fn disk_cold_file_create() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("scrollback/cold/cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    assert!(cold.is_disk_backed());
    assert!(path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let storage_dir_mode = path
            .parent()
            .expect("cold storage directory")
            .metadata()
            .expect("cold storage metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            storage_dir_mode, 0o700,
            "cold storage directory should be 0o700, got 0o{storage_dir_mode:03o}"
        );
    }

    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    assert_eq!(cold.line_count(), 10);
}

#[test]
fn disk_cold_file_reload() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Create and populate
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        let (compressed1, count1) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed1, count1).unwrap();

        let (compressed2, count2) = create_test_page(5, "Page1");
        cold.push_compressed(&compressed2, count2).unwrap();

        cold.sync().unwrap();
    }

    // Reload and verify
    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();

        assert_eq!(cold.line_count(), 10);
        assert_eq!(cold.page_count(), 2);
    }
}

#[test]
fn disk_cold_get_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Add multiple pages
    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    assert_eq!(cold.line_count(), 15);

    // Test line retrieval across pages
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(4)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line4"
    );
    assert_eq!(
        cold.get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page1-Line0"
    );
    assert_eq!(
        cold.get_line(10)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page2-Line0"
    );
    assert_eq!(
        cold.get_line(14)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page2-Line4"
    );
    assert!(cold.get_line(15).expect("no error").is_none());
}

#[test]
fn disk_cold_lru_cache() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path).with_cache_size(2);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Add 5 pages
    for page_num in 0..5 {
        let (compressed, line_count) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    // Access pages 0, 1, 2 - cache should only hold 2
    cold.get_line(0).expect("no error");
    cold.get_line(10).expect("no error");
    cold.get_line(20).expect("no error");

    // Cache should have evicted page 0
    assert!(cold.cache.borrow().len() <= 2);
}

#[test]
fn disk_cold_clear() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    assert_eq!(cold.line_count(), 10);
    // Verify mmap is usable (not just present) before clear
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );
    let file_len_before = cold
        .file
        .as_ref()
        .expect("file after push")
        .metadata()
        .unwrap()
        .len();
    let mmap_len_before = cold
        .mmap
        .as_ref()
        .expect("mmap must be established after push")
        .len() as u64;
    assert!(file_len_before > HEADER_SIZE as u64);
    assert_eq!(mmap_len_before, file_len_before);
    assert!(
        cold.cache_bytes.get() > 0,
        "cache_bytes should track cached pages"
    );

    cold.clear().unwrap();

    assert_eq!(cold.line_count(), 0);
    assert_eq!(cold.page_count(), 0);
    assert!(cold.cache.borrow().is_empty());
    assert_eq!(cold.cache_bytes.get(), 0);
    assert!(cold.mmap.is_none());
    assert_eq!(cold.access_counter.get(), 0);
    assert_eq!(cold.write_offset, HEADER_SIZE as u64);
    let file_len_after = cold.file.as_ref().unwrap().metadata().unwrap().len();
    assert_eq!(file_len_after, HEADER_SIZE as u64);
}

#[test]
fn disk_cold_clear_remap() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();
    cold.clear().unwrap();
    assert!(cold.mmap.is_none());

    let (compressed2, line_count2) = create_test_page(5, "Page1");
    cold.push_compressed(&compressed2, line_count2).unwrap();

    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page1-Line0"
    );
    let file_len = cold.file.as_ref().unwrap().metadata().unwrap().len();
    let mmap_len = cold
        .mmap
        .as_ref()
        .expect("mmap must be re-established after clear+push")
        .len() as u64;
    assert_eq!(mmap_len, file_len);
}

#[test]
fn disk_cold_cache_byte_limit_bounds_repeated_reads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path)
        .with_cache_size(4)
        .with_cache_byte_limit(250 * 1024);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..4 {
        let (compressed, line_count) =
            create_large_test_page(120 * 1024, &format!("LargePage{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    for page_num in 0..4 {
        let line = cold
            .get_line(page_num)
            .expect("no error")
            .expect("line present");
        assert!(
            line.to_string()
                .starts_with(&format!("LargePage{page_num}-")),
            "wrong page returned for cached read"
        );
        assert!(
            cold.cache_bytes.get() <= cold.cache_byte_limit,
            "cache bytes {} exceeded byte limit {}",
            cold.cache_bytes.get(),
            cold.cache_byte_limit
        );
    }

    assert!(
        cold.cache.borrow().len() <= 2,
        "byte cap should evict some of the 4 accessed pages"
    );
}

#[test]
fn disk_cold_skips_cache_for_oversized_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path)
        .with_cache_size(2)
        .with_cache_byte_limit(64 * 1024);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_large_test_page(256 * 1024, "Oversized");
    cold.push_compressed(&compressed, line_count).unwrap();

    let line = cold.get_line(0).expect("no error").expect("line present");
    assert!(
        line.to_string().starts_with("Oversized-"),
        "oversized page should still be readable"
    );
    assert!(
        cold.cache.borrow().is_empty(),
        "oversized page must not be cached"
    );
    assert_eq!(cold.cache_bytes.get(), 0);
}

#[test]
fn disk_cold_clear_persists_empty_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        let (compressed, line_count) = create_test_page(10, "Page0");
        cold.push_compressed(&compressed, line_count).unwrap();
        cold.clear().unwrap();
        let (page_count, line_count) = read_header_counts(&path);
        assert_eq!(page_count, 0);
        assert_eq!(line_count, 0);
        let file_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(file_len, HEADER_SIZE as u64);
    }

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(cold.line_count(), 0);
    assert_eq!(cold.page_count(), 0);
    assert!(cold.is_empty());
}

#[test]
fn disk_cold_drop_releases_mmap() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        let (compressed, line_count) = create_test_page(10, "Page0");
        cold.push_compressed(&compressed, line_count).unwrap();
        // Verify mmap content is readable before drop
        assert_eq!(
            cold.get_line(0)
                .expect("no error")
                .expect("line present")
                .to_string(),
            "Page0-Line0"
        );
        let file_len = cold
            .file
            .as_ref()
            .expect("file before drop")
            .metadata()
            .unwrap()
            .len();
        let mmap_len = cold
            .mmap
            .as_ref()
            .expect("mmap must exist before drop")
            .len() as u64;
        assert_eq!(mmap_len, file_len);
    }

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn disk_cold_mmap_len_tracks_file_len() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    let file_len = cold
        .file
        .as_ref()
        .expect("file after push")
        .metadata()
        .unwrap()
        .len();
    let mmap_len = cold.mmap.as_ref().expect("mmap after first push").len() as u64;
    assert_eq!(mmap_len, file_len);
    // Verify mmap is usable, not just sized correctly
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );

    let (compressed2, line_count2) = create_test_page(5, "Page1");
    cold.push_compressed(&compressed2, line_count2).unwrap();

    let file_len2 = cold
        .file
        .as_ref()
        .expect("file after second push")
        .metadata()
        .unwrap()
        .len();
    let mmap_len2 = cold.mmap.as_ref().expect("mmap after second push").len() as u64;
    assert_eq!(mmap_len2, file_len2);
    assert!(file_len2 > file_len, "file should grow after second push");
    // Verify content from both pages is readable through mmap
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(10)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page1-Line0"
    );
}

/// Verify `find_page` correctness with non-uniform page sizes.
///
/// The `+1` encoding in `find_page` (`binary_search(&(line_idx + 1))`) is
/// subtle for cumulative prefix sums. This test exercises exact page
/// boundaries where pages have different line counts (3, 7, 2) to verify
/// the encoding handles first-line, last-line, and boundary transitions.
#[test]
fn disk_cold_get_line_non_uniform_page_sizes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Push pages with non-uniform line counts: 3, 7, 2
    // cumulative: [3, 10, 12]
    let (c0, lc0) = create_test_page(3, "A");
    cold.push_compressed(&c0, lc0).unwrap();
    let (c1, lc1) = create_test_page(7, "B");
    cold.push_compressed(&c1, lc1).unwrap();
    let (c2, lc2) = create_test_page(2, "C");
    cold.push_compressed(&c2, lc2).unwrap();

    assert_eq!(cold.line_count(), 12);

    // Page 0 boundaries: lines 0..3
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "A-Line0");
    assert_eq!(cold.get_line(2).unwrap().unwrap().to_string(), "A-Line2");

    // Page 1 boundaries: lines 3..10
    assert_eq!(cold.get_line(3).unwrap().unwrap().to_string(), "B-Line0");
    assert_eq!(cold.get_line(9).unwrap().unwrap().to_string(), "B-Line6");

    // Page 2 boundaries: lines 10..12
    assert_eq!(cold.get_line(10).unwrap().unwrap().to_string(), "C-Line0");
    assert_eq!(cold.get_line(11).unwrap().unwrap().to_string(), "C-Line1");

    // Out of bounds
    assert!(cold.get_line(12).unwrap().is_none());
}

// =========================================================================
// DiskColdTier::truncate_front_lines tests (#5911)
// =========================================================================

/// truncate_front_lines within a single page uses front_offset.
#[test]
fn disk_cold_truncate_front_lines_partial_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-partial.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, count) = create_test_page(5, "P0");
    cold.push_compressed(&compressed, count).unwrap();
    assert_eq!(cold.line_count(), 5);

    // Remove 2 oldest lines.
    cold.truncate_front_lines(2);
    assert_eq!(cold.line_count(), 3);
    assert_eq!(cold.page_count(), 1, "page not yet fully consumed");

    // First available line should be P0-Line2.
    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P0-Line2");
    let line = cold.get_line(2).unwrap().unwrap();
    assert_eq!(line.to_string(), "P0-Line4");
}

/// truncate_front_lines crossing a page boundary drops the consumed page.
#[test]
fn disk_cold_truncate_front_lines_crosses_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-cross.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c1, n1) = create_test_page(5, "P0");
    let (c2, n2) = create_test_page(5, "P1");
    cold.push_compressed(&c1, n1).unwrap();
    cold.push_compressed(&c2, n2).unwrap();
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 2);

    // Remove 7 lines: consumes all of page 0 (5 lines) + 2 from page 1.
    cold.truncate_front_lines(7);
    assert_eq!(cold.line_count(), 3);
    assert_eq!(cold.page_count(), 1, "consumed page should be dropped");

    // First available line should be P1-Line2.
    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P1-Line2");
    let last = cold.get_line(2).unwrap().unwrap();
    assert_eq!(last.to_string(), "P1-Line4");
}

/// truncate_front_lines on exact page boundary drops the page cleanly.
#[test]
fn disk_cold_truncate_front_lines_exact_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-exact.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c1, n1) = create_test_page(5, "P0");
    let (c2, n2) = create_test_page(5, "P1");
    cold.push_compressed(&c1, n1).unwrap();
    cold.push_compressed(&c2, n2).unwrap();

    // Remove exactly 5 (one full page).
    cold.truncate_front_lines(5);
    assert_eq!(cold.line_count(), 5);
    assert_eq!(cold.page_count(), 1, "first page should be dropped");

    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P1-Line0");
}

/// truncate_front_lines removing ALL lines leaves DiskColdTier empty.
#[test]
fn disk_cold_truncate_front_lines_all() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-all.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c1, n1) = create_test_page(5, "P0");
    let (c2, n2) = create_test_page(5, "P1");
    cold.push_compressed(&c1, n1).unwrap();
    cold.push_compressed(&c2, n2).unwrap();
    assert_eq!(cold.line_count(), 10);

    cold.truncate_front_lines(10);
    assert_eq!(cold.line_count(), 0);
    assert_eq!(cold.page_count(), 0, "all pages should be dropped");
    assert!(
        cold.get_line(0).unwrap().is_none(),
        "empty tier returns None"
    );
}

/// Push after truncate_front_lines keeps cumulative_lines consistent.
#[test]
fn disk_cold_truncate_then_push() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-push.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c1, n1) = create_test_page(5, "P0");
    let (c2, n2) = create_test_page(5, "P1");
    cold.push_compressed(&c1, n1).unwrap();
    cold.push_compressed(&c2, n2).unwrap();

    // Truncate first page + 2 lines of second.
    cold.truncate_front_lines(7);
    assert_eq!(cold.line_count(), 3);

    // Push a new page.
    let (c3, n3) = create_test_page(4, "P2");
    cold.push_compressed(&c3, n3).unwrap();
    assert_eq!(cold.line_count(), 7, "3 surviving + 4 new = 7");
    assert_eq!(cold.page_count(), 2, "1 surviving + 1 new = 2");

    // Verify data: first 3 lines from P1, then 4 from P2.
    let line0 = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line0.to_string(), "P1-Line2");
    let line3 = cold.get_line(3).unwrap().unwrap();
    assert_eq!(line3.to_string(), "P2-Line0");
    let line6 = cold.get_line(6).unwrap().unwrap();
    assert_eq!(line6.to_string(), "P2-Line3");
}

/// Repeated small truncations across multiple page boundaries.
#[test]
fn disk_cold_truncate_front_lines_incremental() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("trunc-incr.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // 4 pages of 3 lines each = 12 lines total.
    for i in 0..4 {
        let (c, n) = create_test_page(3, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    assert_eq!(cold.line_count(), 12);
    assert_eq!(cold.page_count(), 4);

    // Remove 2 (partial first page).
    cold.truncate_front_lines(2);
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 4);
    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P0-Line2");

    // Remove 1 more (completes first page).
    cold.truncate_front_lines(1);
    assert_eq!(cold.line_count(), 9);
    assert_eq!(cold.page_count(), 3, "first page now consumed");
    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P1-Line0");

    // Remove 4 (crosses page 1 into page 2).
    cold.truncate_front_lines(4);
    assert_eq!(cold.line_count(), 5);
    assert_eq!(cold.page_count(), 2);
    let line = cold.get_line(0).unwrap().unwrap();
    assert_eq!(line.to_string(), "P2-Line1");
}

// =========================================================================
// Crash recovery tests (#5917)
// =========================================================================

/// Simulate a crash mid-write: file has a complete page followed by a partial
/// page (header written, compressed data truncated). On reload, the partial
/// page should be discarded and the complete page should be accessible.
#[test]
fn disk_cold_crash_recovery_partial_page_discarded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("crash-partial.dtrm");

    // Write one complete page normally.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, count) = create_test_page(5, "Good");
        cold.push_compressed(&compressed, count).unwrap();
        cold.sync().unwrap();
    }

    // Simulate a crash: append a page header claiming 1000 bytes of compressed
    // data, but only write 10 bytes. This mimics a process killed mid-write.
    {
        use std::io::{Seek, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::End(0)).unwrap();
        let fake_compressed_size: u32 = 1000;
        let fake_line_count: u32 = 10;
        file.write_all(&fake_compressed_size.to_le_bytes()).unwrap();
        file.write_all(&fake_line_count.to_le_bytes()).unwrap();
        // Only write 10 bytes of "compressed" data instead of 1000.
        file.write_all(&[0xAB; 10]).unwrap();
        file.flush().unwrap();
    }

    // Reload: partial page should be discarded, complete page intact.
    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();
        assert_eq!(cold.page_count(), 1, "partial page must be discarded");
        assert_eq!(cold.line_count(), 5, "only complete page's lines survive");
        let line = cold.get_line(0).unwrap().unwrap();
        assert_eq!(line.to_string(), "Good-Line0");
        let last = cold.get_line(4).unwrap().unwrap();
        assert_eq!(last.to_string(), "Good-Line4");
    }
}

/// A file with only a partial page header (< PAGE_HEADER_SIZE bytes after the
/// last complete page) should be handled gracefully — zero pages loaded.
#[test]
fn disk_cold_crash_recovery_truncated_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("crash-trunc-hdr.dtrm");

    // Create an empty file with just the file header.
    {
        let config = DiskColdConfig::new(&path);
        let _cold = DiskColdTier::with_config(config).unwrap();
    }

    // Append a few bytes (less than PAGE_HEADER_SIZE) to simulate a crash
    // during the very start of a page write.
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&[0x01, 0x02, 0x03]).unwrap();
        file.flush().unwrap();
    }

    // Reload: no pages should be loaded.
    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();
        assert_eq!(cold.page_count(), 0, "truncated header must be ignored");
        assert_eq!(cold.line_count(), 0);
    }
}

/// Verify that push_compressed uses write-ahead ordering: page data is synced
/// before header counters. After a normal write, header counters match the
/// scanned page data.
#[test]
fn disk_cold_push_compressed_header_consistent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("consistent.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(5, "P0");
        let (c2, n2) = create_test_page(3, "P1");
        cold.push_compressed(&c1, n1).unwrap();
        cold.push_compressed(&c2, n2).unwrap();
        cold.sync().unwrap();
    }

    // Verify header counts match actual page data.
    let (header_pages, header_lines) = read_header_counts(&path);
    assert_eq!(header_pages, 2, "header page count must match");
    assert_eq!(header_lines, 8, "header line count must match (5+3)");

    // Reload and verify data integrity.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(cold.page_count(), 2);
    assert_eq!(cold.line_count(), 8);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P0-Line0");
    assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "P1-Line0");
}

// Compaction tests extracted to disk_compaction_tests.rs
#[path = "disk_compaction_tests.rs"]
mod compaction;

/// Regression: #5923 — `create()` must sync header to disk immediately.
///
/// Before the fix, `create()` called `file.flush()` which is a no-op on
/// `std::fs::File`. After the fix, `sync_data()` ensures the header is
/// durable on disk without waiting for `Drop`.
#[test]
fn disk_cold_create_header_durable_on_disk() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let _cold = DiskColdTier::with_config(config).unwrap();

    // Read the on-disk header directly (bypassing the tier API).
    // sync_data() in create() ensures this is visible immediately.
    let (page_count, line_count) = read_header_counts(&path);
    assert_eq!(page_count, 0, "freshly created header should have 0 pages");
    assert_eq!(line_count, 0, "freshly created header should have 0 lines");
}

/// Regression: #5923 — `clear()` must sync zeroed header to disk immediately.
///
/// Before the fix, `clear()` called `file.flush()` which is a no-op on
/// `std::fs::File`. After the fix, `sync_data()` ensures the zeroed header
/// is durable without waiting for `Drop::sync_all()`.
#[test]
fn disk_cold_clear_header_durable_on_disk() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Push data so header is non-zero
    let (compressed, count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, count).unwrap();

    let (pages_before, lines_before) = read_header_counts(&path);
    assert_eq!(pages_before, 1, "should have 1 page before clear");
    assert_eq!(lines_before, 10, "should have 10 lines before clear");

    // Clear resets header to zero and syncs to disk
    cold.clear().unwrap();

    // Verify on-disk header is zeroed immediately (not deferred to Drop)
    let (pages_after, lines_after) = read_header_counts(&path);
    assert_eq!(pages_after, 0, "clear() must zero page count on disk");
    assert_eq!(lines_after, 0, "clear() must zero line count on disk");
}

// Performance regression tests — drain + single-line extraction (P10 6001)

/// Exercises drain(..k) path that replaced O(k*n) remove(0) loops.
#[test]
fn disk_cold_truncate_multi_page_drain() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("drain.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for i in 0..8 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }
    assert_eq!((cold.line_count(), cold.page_count()), (40, 8));

    // Remove 27 lines — consumes 5 full pages, offset 2 into P5.
    cold.truncate_front_lines(27);
    assert_eq!((cold.line_count(), cold.page_count()), (13, 3));
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P5-Line2");
    assert_eq!(cold.get_line(12).unwrap().unwrap().to_string(), "P7-Line4");
    assert!(cold.get_line(13).unwrap().is_none());
}

/// Exercises load_line() single-line extraction path (cache miss then hits).
#[test]
fn disk_cold_get_line_cache_hit_single_extraction() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("cache-hit.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c, n) = create_test_page(10, "Pg");
    cold.push_compressed(&c, n).unwrap();

    // Cache miss then cache hits — each returns correct single line.
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "Pg-Line0");
    assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "Pg-Line5");
    assert_eq!(cold.get_line(9).unwrap().unwrap().to_string(), "Pg-Line9");
    assert!(cold.get_line(10).unwrap().is_none());
}
