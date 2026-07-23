// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reload/repair tests after simulated crashes for `DiskColdTier`.

use super::*;

#[test]
fn disk_cold_reload_and_read() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Create and populate
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        for page_num in 0..3 {
            let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
            cold.push_compressed(&compressed, line_count).unwrap();
        }

        cold.sync().unwrap();
    }

    // Reload and read lines
    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();

        assert_eq!(cold.line_count(), 15);
        assert_eq!(
            cold.get_line(0)
                .expect("no error")
                .expect("line present")
                .to_string(),
            "Page0-Line0"
        );
        assert_eq!(
            cold.get_line(7)
                .expect("no error")
                .expect("line present")
                .to_string(),
            "Page1-Line2"
        );
        assert_eq!(
            cold.get_line(14)
                .expect("no error")
                .expect("line present")
                .to_string(),
            "Page2-Line4"
        );
    }
}

#[test]
fn disk_cold_reload_discards_trailing_partial_page_and_repairs_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let valid_file_len = {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, line_count) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed, line_count).unwrap();
        cold.sync().unwrap();
        cold.file.as_ref().unwrap().metadata().unwrap().len()
    };

    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let (crash_page, crash_line_count) = create_test_page(5, "Crash");
        let partial_len = crash_page.len().min(3);

        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(&9u64.to_le_bytes()).unwrap();
        file.write_all(&99u64.to_le_bytes()).unwrap();

        file.seek(SeekFrom::End(0)).unwrap();
        let crash_page_size = u32::try_from(crash_page.len()).unwrap();
        let crash_page_lines = u32::try_from(crash_line_count).unwrap();
        file.write_all(&crash_page_size.to_le_bytes()).unwrap();
        file.write_all(&crash_page_lines.to_le_bytes()).unwrap();
        file.write_all(&crash_page[..partial_len]).unwrap();
        file.sync_all().unwrap();
    }

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(cold.line_count(), 5);
    assert_eq!(cold.page_count(), 1);
    assert_eq!(
        cold.get_line(4)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line4"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        valid_file_len,
        "load should truncate the trailing partial page"
    );
    assert_eq!(
        read_header_counts(&path),
        (1, 5),
        "load should rewrite header counters from recovered pages"
    );
}

/// Simulates crash after page data is durable but before header update.
/// scan_pages should discover the orphaned page and repair the header.
#[test]
fn disk_cold_reload_recovers_page_with_stale_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Write one valid page via the normal API.
    let (page0_compressed, page0_lines) = create_test_page(5, "Page0");
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        cold.push_compressed(&page0_compressed, page0_lines)
            .unwrap();
        cold.sync().unwrap();
    }

    // Simulate crash: append a complete second page to the file
    // but leave the header counters at 1 page / 5 lines (stale).
    let (page1_compressed, page1_lines) = create_test_page(5, "Page1");
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // Append page header + full compressed data at end of file.
        file.seek(SeekFrom::End(0)).unwrap();
        let compressed_size = u32::try_from(page1_compressed.len()).unwrap();
        let line_count_u32 = u32::try_from(page1_lines).unwrap();
        file.write_all(&compressed_size.to_le_bytes()).unwrap();
        file.write_all(&line_count_u32.to_le_bytes()).unwrap();
        file.write_all(&page1_compressed).unwrap();
        file.sync_all().unwrap();
        // Header still says 1 page, 5 lines — not updated.
    }

    // Reload — scan_pages should find both pages and repair header.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(
        cold.page_count(),
        2,
        "scan_pages should discover orphaned page"
    );
    assert_eq!(
        cold.line_count(),
        10,
        "line count should include both pages"
    );
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(5)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page1-Line0"
    );
    assert_eq!(
        read_header_counts(&path),
        (2, 10),
        "header should be repaired to match scanned pages"
    );
}

/// Simulates a crash where the page header reaches disk but the compressed
/// payload is corrupted. load() should discard the bad trailing page.
#[test]
fn disk_cold_reload_truncates_page_with_corrupt_zstd_magic() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let valid_file_len = {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (page0_compressed, page0_lines) = create_test_page(5, "Page0");
        cold.push_compressed(&page0_compressed, page0_lines)
            .unwrap();
        cold.sync().unwrap();
        let file_len = cold.file.as_ref().unwrap().metadata().unwrap().len();

        let (page1_compressed, page1_lines) = create_test_page(5, "Page1");
        cold.push_compressed(&page1_compressed, page1_lines)
            .unwrap();
        cold.sync().unwrap();
        file_len
    };

    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(valid_file_len + PAGE_HEADER_SIZE as u64))
            .unwrap();
        file.write_all(&[0u8; 4]).unwrap();
        file.sync_all().unwrap();
    }

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(
        cold.page_count(),
        1,
        "corrupt trailing page should be discarded"
    );
    assert_eq!(cold.line_count(), 5);
    assert_eq!(
        cold.get_line(4)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line4"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        valid_file_len,
        "load should truncate the corrupt trailing page"
    );
    assert_eq!(
        read_header_counts(&path),
        (1, 5),
        "header should be repaired after dropping the corrupt page"
    );
}

/// Simulates crash mid-write: page header written with zero compressed_size.
/// scan_pages should stop at the zero-sized header and truncate.
#[test]
fn disk_cold_reload_truncates_zero_size_page_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let valid_file_len = {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, line_count) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed, line_count).unwrap();
        cold.sync().unwrap();
        cold.file.as_ref().unwrap().metadata().unwrap().len()
    };

    // Append a page header with compressed_size=0 (partially-written crash).
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap(); // compressed_size = 0
        file.write_all(&5u32.to_le_bytes()).unwrap(); // line_count = 5
        file.sync_all().unwrap();
    }

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(cold.page_count(), 1, "zero-size page should be discarded");
    assert_eq!(cold.line_count(), 5);
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        valid_file_len,
        "file should be truncated to remove zero-size page header"
    );
}

/// A corrupt/torn `page_count` header field (e.g. all-0xFF left by a partial
/// header write or filesystem corruption) must NOT be trusted as an allocation
/// hint. Previously it flowed straight into `Vec::with_capacity(usize::MAX)` in
/// `scan_pages`, aborting load() with 'capacity overflow'. `scan_pages` now
/// clamps the hint to the page headers the file can physically hold, so load()
/// stays panic-free and the rescan still recovers the true counts.
#[test]
fn disk_cold_load_corrupt_page_count_does_not_panic() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Write two valid pages via the normal API.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        for page_num in 0..2 {
            let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
            cold.push_compressed(&compressed, line_count).unwrap();
        }
        cold.sync().unwrap();
    }

    // Corrupt the page_count field (header bytes [8..16]) to u64::MAX — the
    // value a torn write / filesystem corruption can leave behind. MAGIC and
    // VERSION remain valid so validate_header() passes and load() proceeds.
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(&u64::MAX.to_le_bytes()).unwrap();
        file.sync_all().unwrap();
    }

    // load() must not panic/abort; the rescan ignores the bogus hint and
    // recovers the true page/line counts from the on-disk page headers.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert_eq!(cold.page_count(), 2, "rescan recovers the true page count");
    assert_eq!(cold.line_count(), 10);
    assert_eq!(
        cold.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(9)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Page1-Line4"
    );
}

/// load() cleans up orphaned `.dtrm.compact` temp file from a prior crash.
#[test]
fn disk_cold_load_cleans_orphan_temp_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    let tmp_path = path.with_extension("dtrm.compact");

    // Create a valid cold tier file.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, lc) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed, lc).unwrap();
        cold.sync().unwrap();
    }

    // Simulate crash: leave an orphaned compaction temp file.
    std::fs::write(&tmp_path, b"orphaned compaction data").unwrap();
    assert!(tmp_path.exists());

    // Reload — load() should clean up the orphaned temp file.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert!(
        !tmp_path.exists(),
        "orphaned .dtrm.compact file should be deleted on load"
    );
    assert_eq!(cold.line_count(), 5);
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
}

/// load() treats orphan-temp cleanup as best-effort and still reloads the main
/// cold-tier file if the temp path cannot be removed.
#[test]
fn disk_cold_load_ignores_non_removable_orphan_temp_path() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    let tmp_path = path.with_extension("dtrm.compact");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, lc) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed, lc).unwrap();
        cold.sync().unwrap();
    }

    std::fs::create_dir(&tmp_path).unwrap();
    assert!(tmp_path.is_dir());

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();

    assert!(
        tmp_path.is_dir(),
        "non-removable orphan temp path should remain"
    );
    assert_eq!(cold.line_count(), 5);
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
}
