// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Truncate_back_lines tests for `DiskColdTier`.

use super::*;

#[test]
fn disk_cold_cache_size_zero_no_infinite_loop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // with_cache_size(0) should be clamped to 1
    let config = DiskColdConfig::new(&path).with_cache_size(0);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(5, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    // Reading triggers cache_page — must not hang
    let line = cold.get_line(0).expect("no error").expect("line present");
    assert_eq!(line.to_string(), "Page0-Line0");

    // Cache holds exactly 1 entry (clamped from 0)
    assert_eq!(cold.cache.borrow().len(), 1);
}

/// truncate_back_lines: remove a few lines from the newest page.
#[test]
fn disk_cold_truncate_back_lines_partial_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }
    assert_eq!(cold.line_count(), 15);

    // Remove 3 lines from the back of the last page.
    cold.truncate_back_lines(3).unwrap();
    assert_eq!(cold.line_count(), 12);
    assert_eq!(cold.page_count(), 3); // boundary page re-pushed

    // Verify first page intact.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
    // Last surviving line should be Page2-Line1 (index 11).
    assert_eq!(
        cold.get_line(11).unwrap().unwrap().to_string(),
        "Page2-Line1"
    );
    assert!(cold.get_line(12).unwrap().is_none());
}

/// truncate_back_lines: remove entire pages from the back.
#[test]
fn disk_cold_truncate_back_lines_whole_pages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    // Remove exactly 10 lines = 2 whole pages from back.
    cold.truncate_back_lines(10).unwrap();
    assert_eq!(cold.line_count(), 5);
    assert_eq!(cold.page_count(), 1);

    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(4).unwrap().unwrap().to_string(),
        "Page0-Line4"
    );
    assert!(cold.get_line(5).unwrap().is_none());

    // File should be truncated to just header + one page.
    let (page_count, line_count) = read_header_counts(&path);
    assert_eq!(page_count, 1);
    assert_eq!(line_count, 5);
}

/// truncate_back_lines with corrupt boundary page must fail without changing state.
/// Mirrors `truncate_back_lines_aborts_on_corrupt_boundary_page` from cold_tier_tests.rs
/// but exercises the disk-backed code path (mmap read + file truncate + re-push).
#[test]
fn disk_cold_truncate_back_lines_aborts_on_corrupt_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }
    assert_eq!(cold.line_count(), 15);
    assert_eq!(cold.page_count(), 3);

    let line_count_before = cold.line_count();
    let page_count_before = cold.page_count();
    let file_size_before = std::fs::metadata(&path).unwrap().len();
    let oldest_line = cold.get_line(0).unwrap().unwrap().to_string();

    // Corrupt the Zstd frame magic of the last page's compressed data on disk.
    // The last page starts at the last index entry's offset.
    let last_entry = &cold.index[cold.index.len() - 1];
    let data_offset = last_entry.offset + PAGE_HEADER_SIZE as u64;
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(data_offset)).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        file.sync_all().unwrap();
    }

    // Refresh mmap so decompress_page reads the corrupted data.
    cold.clear_cache();
    cold.mmap = {
        let file = cold.file.as_ref().unwrap();
        Some(unsafe { crate::mmap::MmapMut::map_mut(file).unwrap() })
    };

    // Verify corruption is visible — last page decompression must fail.
    assert!(
        cold.get_line(line_count_before - 1).is_err(),
        "corruption must affect the last page"
    );

    // truncate_back_lines(3) targets the boundary (last) page — should fail.
    let result = cold.truncate_back_lines(3);
    assert!(
        result.is_err(),
        "truncate_back_lines should fail on corrupt boundary page"
    );

    // Error safety: state must be unchanged.
    assert_eq!(
        cold.line_count(),
        line_count_before,
        "line_count changed on decompression failure"
    );
    assert_eq!(
        cold.page_count(),
        page_count_before,
        "page_count changed on decompression failure"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        file_size_before,
        "file size changed on decompression failure"
    );

    // Older pages must still be accessible.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        oldest_line,
        "oldest page data should be intact after failed truncate"
    );
}

/// Transactional back-truncation (#4638): a failed `truncate_back_lines` must
/// leave the tier in a state where a SUBSEQUENT operation still works. Because
/// the refactored truncate performs all fallible work (decompress/re-compress
/// and every file write) BEFORE committing any in-memory index/cumulative_lines
/// mutation, an aborted boundary trim leaves no partial state. We prove this by
/// failing a partial-page trim (which decompresses the corrupt boundary page),
/// then dropping that page WHOLESALE (which does not decompress it) — the
/// wholesale drop must succeed and the surviving pages stay readable.
#[test]
fn disk_cold_failed_back_truncate_leaves_state_recoverable() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }
    assert_eq!(cold.line_count(), 15);

    // Corrupt the Zstd magic of the last page's compressed data on disk.
    let last_entry = &cold.index[cold.index.len() - 1];
    let data_offset = last_entry.offset + PAGE_HEADER_SIZE as u64;
    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(data_offset)).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        file.sync_all().unwrap();
    }
    cold.clear_cache();
    cold.mmap = {
        let file = cold.file.as_ref().unwrap();
        Some(unsafe { crate::mmap::MmapMut::map_mut(file).unwrap() })
    };

    // Partial trim targets the corrupt boundary page -> decompress fails ->
    // Err with NO state mutation (transactional Phase 1).
    assert!(
        cold.truncate_back_lines(3).is_err(),
        "partial trim of a corrupt boundary page must fail"
    );
    assert_eq!(
        cold.line_count(),
        15,
        "failed trim must not change line_count"
    );
    assert_eq!(
        cold.page_count(),
        3,
        "failed trim must not change page_count"
    );

    // Dropping the corrupt page WHOLESALE never decompresses it, so it must
    // succeed — and it only could if the prior failure left a consistent index.
    cold.truncate_back_lines(5).unwrap();
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.page_count(), 2);
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
    assert_eq!(
        cold.get_line(9).unwrap().unwrap().to_string(),
        "Page1-Line4"
    );
    assert!(cold.get_line(10).unwrap().is_none());

    // Header on disk must reflect the surviving pages.
    let (page_count, line_count) = read_header_counts(&path);
    assert_eq!((page_count, line_count), (2, 10));
}

/// truncate_back_lines persists correct state: reload after partial truncate
/// must reflect the truncated state.
#[test]
fn disk_cold_truncate_back_lines_persists_on_reload() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        for page_num in 0..4 {
            let (compressed, lc) = create_test_page(5, &format!("Page{page_num}"));
            cold.push_compressed(&compressed, lc).unwrap();
        }
        assert_eq!(cold.line_count(), 20);

        // Remove 7 lines from back: drops Page3 (5 lines) + trims 2 from Page2.
        cold.truncate_back_lines(7).unwrap();
        assert_eq!(cold.line_count(), 13);
    }

    // Reload and verify persisted state.
    let config = DiskColdConfig::new(&path);
    let reloaded = DiskColdTier::with_config(config).unwrap();

    assert_eq!(reloaded.line_count(), 13);
    assert_eq!(
        reloaded.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
    // Last line: Page2-Line2 (3 lines kept from the boundary page).
    assert_eq!(
        reloaded.get_line(12).unwrap().unwrap().to_string(),
        "Page2-Line2"
    );
    assert!(reloaded.get_line(13).unwrap().is_none());
}

/// `truncate_front_lines` skips the full-cache accounting walk when no page
/// is dropped (the steady-state per-line case), yet `bytes_used` must remain
/// EXACTLY equal to `calculate_memory_used()` — with a warm cache — both when
/// no page boundary is crossed and when pages are dropped (cache invalidated).
#[test]
fn disk_cold_front_truncate_bytes_used_exact_without_page_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }
    // Warm the decompressed-page cache so its term is non-trivial.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page0-Line0"
    );
    assert!(!cold.cache.borrow().is_empty());
    assert_eq!(cold.memory_used(), cold.recompute_memory_used());

    // No page boundary crossed: pages_dropped == 0, cache stays warm,
    // and bytes_used must still be exact without a recompute.
    cold.truncate_front_lines(2);
    assert_eq!(cold.line_count(), 13);
    assert!(!cold.cache.borrow().is_empty(), "cache must survive");
    assert_eq!(cold.memory_used(), cold.recompute_memory_used());

    // Cross a page boundary: Page0 (5 lines) is fully consumed after 2+3,
    // the cache is invalidated and bytes_used must be refreshed.
    cold.truncate_front_lines(3);
    assert_eq!(cold.line_count(), 10);
    assert!(cold.cache.borrow().is_empty(), "cache must be invalidated");
    assert_eq!(cold.memory_used(), cold.recompute_memory_used());

    // Surviving data still readable after both truncations.
    assert_eq!(
        cold.get_line(0).unwrap().unwrap().to_string(),
        "Page1-Line0"
    );
    assert_eq!(cold.memory_used(), cold.recompute_memory_used());
}

/// Verify `get_line` at exact page boundaries after `truncate_front_lines`
/// creates a non-zero `front_offset` with non-uniform page sizes.
///
/// This is the trickiest combination for `find_page`: the `+1` encoding
/// must be correct when `physical_idx = logical_idx + front_offset` lands
/// exactly on a page boundary in a non-uniform cumulative array.
#[test]
fn disk_cold_get_line_boundaries_after_front_truncate_non_uniform() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Pages: 4 lines, 6 lines, 3 lines, 5 lines = 18 total
    let (c0, lc0) = create_test_page(4, "P0");
    cold.push_compressed(&c0, lc0).unwrap();
    let (c1, lc1) = create_test_page(6, "P1");
    cold.push_compressed(&c1, lc1).unwrap();
    let (c2, lc2) = create_test_page(3, "P2");
    cold.push_compressed(&c2, lc2).unwrap();
    let (c3, lc3) = create_test_page(5, "P3");
    cold.push_compressed(&c3, lc3).unwrap();
    assert_eq!(cold.line_count(), 18);

    // Truncate 7 lines from front:
    // - Page P0 (4 lines): fully consumed, pages_dropped = 1, remaining = 3
    // - Page P1 (6 lines): 3 < 6, so front_offset = 3
    // After: front_offset = 3, surviving pages = [P1(6), P2(3), P3(5)]
    // cumulative after drain+adjust = [6, 9, 14]
    // logical line 0 = P1-Line3 (physical_idx = 0 + 3 = 3)
    cold.truncate_front_lines(7);
    assert_eq!(cold.line_count(), 11);

    // First surviving line: P1-Line3 (logical 0)
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P1-Line3");

    // Last line of surviving P1: P1-Line5 (logical 2, physical 5)
    assert_eq!(cold.get_line(2).unwrap().unwrap().to_string(), "P1-Line5");

    // First line of P2: P2-Line0 (logical 3, physical 6)
    assert_eq!(cold.get_line(3).unwrap().unwrap().to_string(), "P2-Line0");

    // Last line of P2: P2-Line2 (logical 5, physical 8)
    assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "P2-Line2");

    // First line of P3: P3-Line0 (logical 6, physical 9)
    assert_eq!(cold.get_line(6).unwrap().unwrap().to_string(), "P3-Line0");

    // Last line: P3-Line4 (logical 10, physical 13)
    assert_eq!(cold.get_line(10).unwrap().unwrap().to_string(), "P3-Line4");

    // Out of bounds
    assert!(cold.get_line(11).unwrap().is_none());
}
