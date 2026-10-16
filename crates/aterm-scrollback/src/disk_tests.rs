// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for `DiskColdTier`.

use super::super::line::serialize_lines;
use super::*;
use crate::ScrollbackError;
use aterm_tempfile::tempdir;
use std::fs::File;
use std::io::Read;

impl DiskColdTier {
    /// Get the number of live pages (front-dropped dead prefix excluded).
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.live_index().len()
    }

    /// Check if empty (no live pages).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live_index().is_empty()
    }

    /// Check if disk-backed.
    #[must_use]
    pub fn is_disk_backed(&self) -> bool {
        self.file.is_some()
    }

    /// Point the last index entry at an arbitrary range, simulating a malformed
    /// `PageIndexEntry` or an out-of-band file truncation.
    #[cfg(test)]
    fn corrupt_last_entry_range(&mut self, offset: u64, compressed_size: u32) {
        if let Some(entry) = self.index.last_mut() {
            entry.offset = offset;
            entry.compressed_size = compressed_size;
        }
    }

    /// Test-only: invoke the page decompression path directly.
    #[cfg(test)]
    fn decompress_page_for_test(&self, page_idx: usize) -> Result<Vec<Line>, ScrollbackError> {
        self.decompress_page(page_idx)
    }
}

fn create_test_page(line_count: usize, prefix: &str) -> (Vec<u8>, usize) {
    let lines: Vec<Line> = (0..line_count)
        .map(|i| Line::from(&*format!("{prefix}-Line{i}")))
        .collect();
    let serialized = serialize_lines(&lines);
    let compressed = crate::encode_cold_block(&serialized).unwrap();
    (compressed, line_count)
}

#[test]
fn disk_cold_cache_byte_limit_is_enforced() {
    // The cold LRU cache evicts by bytes, not only page count: with a large
    // cache_size only the byte budget binds, and cache_bytes never exceeds it.
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    let config = DiskColdConfig::new(&path)
        .with_cache_size(64)
        .with_cache_byte_limit(50 * 1024);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let num_pages = 24usize;
    for p in 0..num_pages {
        let (compressed, lc) = create_test_page(100, &format!("P{p}"));
        cold.push_compressed(&compressed, lc).unwrap();
    }
    for p in 0..num_pages {
        let _ = cold.get_line(p * 100).expect("no read error");
        assert!(
            cold.cache_bytes.get() <= cold.cache_byte_limit,
            "cache_bytes {} must never exceed cache_byte_limit {}",
            cold.cache_bytes.get(),
            cold.cache_byte_limit,
        );
    }
    // The byte budget, not the page-count cap, forces eviction.
    assert!(
        cold.cache.borrow().len() < num_pages,
        "the byte budget must force eviction below the page-count cap"
    );
    // cache_bytes stays exactly the sum of per-entry byte sizes.
    let sum: usize = cold.cache.borrow().values().map(|e| e.bytes).sum();
    assert_eq!(
        cold.cache_bytes.get(),
        sum,
        "cache_bytes must equal the sum of entry sizes"
    );
}

#[test]
fn disk_cold_skips_caching_an_oversized_page() {
    // A page larger than the whole byte budget is not cached (a re-read
    // re-decompresses).
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    let config = DiskColdConfig::new(&path)
        .with_cache_size(4)
        .with_cache_byte_limit(1024);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, lc) = create_test_page(2000, "Big");
    cold.push_compressed(&compressed, lc).unwrap();

    let line = cold
        .get_line(0)
        .expect("no read error")
        .expect("line present");
    assert!(
        line.to_string().starts_with("Big-"),
        "oversized page must still be readable"
    );
    assert!(
        cold.cache.borrow().is_empty(),
        "oversized page must not be cached"
    );
    assert_eq!(cold.cache_bytes.get(), 0);
}

fn read_header_counts(path: &Path) -> (u64, u64) {
    let mut file = File::open(path).unwrap();
    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header).unwrap();
    let page_count = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let line_count = u64::from_le_bytes(header[16..24].try_into().unwrap());
    (page_count, line_count)
}

/// In-memory mode is metadata-only by design: counts track, data is not stored.
#[test]
fn disk_cold_in_memory() {
    let mut cold = DiskColdTier::new();
    assert!(cold.is_empty());
    assert!(!cold.is_disk_backed());

    let (compressed, line_count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    // Metadata only: reads fail.
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

    // Metadata accumulates across pushes.
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

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();

        let (compressed1, count1) = create_test_page(5, "Page0");
        cold.push_compressed(&compressed1, count1).unwrap();

        let (compressed2, count2) = create_test_page(5, "Page1");
        cold.push_compressed(&compressed2, count2).unwrap();

        cold.sync().unwrap();
    }

    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();

        assert_eq!(cold.line_count(), 10);
        assert_eq!(cold.page_count(), 2);
    }
}

/// A corrupt `page_count` header field must not panic the reload with a
/// `Vec::with_capacity` overflow: it is only a preallocation hint, and
/// `scan_pages` rebuilds the index from the page headers.
#[test]
fn corrupt_page_count_header_does_not_panic_on_reload() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Two real pages, then close the file.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(3, "A");
        cold.push_compressed(&c1, n1).unwrap();
        let (c2, n2) = create_test_page(4, "B");
        cold.push_compressed(&c2, n2).unwrap();
        cold.sync().unwrap();
    }

    // Corrupt only the page_count field (header[8..16]) to u64::MAX.
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(8)).unwrap();
        f.write_all(&u64::MAX.to_le_bytes()).unwrap();
        f.flush().unwrap();
    }

    // The page-header scan recovers both real pages regardless of the count.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(
        cold.page_count(),
        2,
        "both real pages must be recovered from the page-header scan despite a corrupt page_count"
    );
}

/// A version-1 `.dtrm` (zstd pages, before the cold codec became LZ4) is
/// refused at load rather than handed to the LZ4 page decoder; the same file at
/// the current version loads, so the refusal is the version field alone.
#[test]
fn version_1_store_is_refused_at_load() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");
    {
        let mut cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
        let (c, n) = create_test_page(3, "A");
        cold.push_compressed(&c, n).unwrap();
        cold.sync().unwrap();
    }
    let reloaded = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    assert_eq!(reloaded.page_count(), 1);
    drop(reloaded);

    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(4)).unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.flush().unwrap();
    }
    let err = DiskColdTier::with_config(DiskColdConfig::new(&path))
        .expect_err("a version-1 store must not load");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// `scan_pages` caps its index preallocation at `MAX_PREALLOC_PAGES`: the
/// `(file_len − 32)/8` page bound scales with the logical length, which a sparse
/// file makes cheap to inflate, so a crafted header (page_count = u64::MAX) on a
/// large sparse file would reserve ~3× its length before the scan stopped.
///
/// The 1 GiB sparse tail clamps the reservation from ~3.2 GB to ~25 MB; the
/// all-zero tail ends the scan right after the two real pages. Sparse file and
/// lazy mmap, so nothing here touches real memory.
#[test]
fn scan_pages_bounds_prealloc_on_large_sparse_file() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Two real pages at the start, then flush/close.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(3, "A");
        cold.push_compressed(&c1, n1).unwrap();
        let (c2, n2) = create_test_page(4, "B");
        cold.push_compressed(&c2, n2).unwrap();
        cold.sync().unwrap();
    }

    // Corrupt page_count to u64::MAX and extend the file to a 1 GiB sparse tail.
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(8)).unwrap();
        f.write_all(&u64::MAX.to_le_bytes()).unwrap();
        f.set_len(1024 * 1024 * 1024).unwrap(); // 1 GiB sparse
        f.flush().unwrap();
    }

    // Reload must not run out of memory; the scan recovers the two real pages.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(
        cold.page_count(),
        2,
        "both real pages recovered despite the lying page_count + huge sparse tail"
    );
    assert_eq!(cold.line_count(), 7, "3 + 4 lines from the two real pages");
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "A-Line0");
    assert_eq!(cold.get_line(6).unwrap().unwrap().to_string(), "B-Line3");
    assert!(
        cold.get_line(7).unwrap().is_none(),
        "no line past the real data"
    );
}

/// A legacy `.dtrm` page from a larger-`block_size` build can declare more lines
/// than the decoder reconstructs (`MAX_DECODE_PAGE_LINES`). `scan_pages` clamps
/// its recorded `line_count` on load so `get_line` never addresses the
/// undecodable suffix. This build never writes such a page (block_size is
/// clamped to the cap).
#[test]
fn scan_pages_clamps_legacy_oversized_page_line_count() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("legacy_oversized.dtrm");
    let oversized = crate::line::MAX_DECODE_PAGE_LINES + 500_000;
    {
        let mut cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
        let (compressed, _) = create_test_page(4, "L");
        // Declare far more lines than the decoder will reconstruct (legacy page).
        cold.push_compressed(&compressed, oversized).unwrap();
        cold.sync().unwrap();
    }
    // Reopen: scan_pages clamps the recorded line_count to the decode cap.
    let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    assert_eq!(
        cold.line_count(),
        crate::line::MAX_DECODE_PAGE_LINES,
        "an oversized legacy page's line_count is clamped to the decode cap on load"
    );
}

/// `scan_pages` stops at a page whose `line_count` is 0: its `cumulative_lines`
/// entry would duplicate the previous total and break the strictly increasing
/// order `find_page`'s binary search relies on. Such a page (with nonzero
/// compressed_size) only comes from truncation or corruption: end of data.
#[test]
fn scan_pages_stops_at_zero_line_page() {
    use std::io::Write;
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Two real pages (10 + 15 lines), then flush/close.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(10, "A");
        cold.push_compressed(&c1, n1).unwrap();
        let (c2, n2) = create_test_page(15, "B");
        cold.push_compressed(&c2, n2).unwrap();
        cold.sync().unwrap();
    }

    // Append a page {compressed_size = 4, line_count = 0} with 4 payload bytes, so
    // the page_end bounds check passes and the line_count guard decides.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let mut header = [0u8; PAGE_HEADER_SIZE];
        header[0..4].copy_from_slice(&4u32.to_le_bytes()); // compressed_size = 4
        header[4..8].copy_from_slice(&0u32.to_le_bytes()); // line_count = 0
        f.write_all(&header).unwrap();
        f.write_all(&[0xAB, 0xCD, 0xEF, 0x00]).unwrap();
        f.flush().unwrap();
    }

    // Reload: the zero-line page (and anything after it) must be dropped.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(
        cold.page_count(),
        2,
        "zero-line page must be excluded from the rebuilt index"
    );
    assert_eq!(
        cold.line_count(),
        25,
        "line count must reflect only the two real pages"
    );

    // cumulative_lines stayed strictly increasing, so every real line resolves.
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "A-Line0");
    assert_eq!(cold.get_line(24).unwrap().unwrap().to_string(), "B-Line14");
    assert!(
        cold.get_line(25).unwrap().is_none(),
        "no line past the real data"
    );
}

#[test]
fn disk_cold_get_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    for page_num in 0..3 {
        let (compressed, line_count) = create_test_page(5, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    assert_eq!(cold.line_count(), 15);

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

    for page_num in 0..5 {
        let (compressed, line_count) = create_test_page(10, &format!("Page{page_num}"));
        cold.push_compressed(&compressed, line_count).unwrap();
    }

    // Touch pages 0, 1, 2 with room for 2.
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
    // Appends never map: a fresh store reads positionally until a load or
    // compaction maps it.
    assert!(cold.mmap.is_none());
    assert!(file_len_before > HEADER_SIZE as u64);

    cold.clear().unwrap();

    assert_eq!(cold.line_count(), 0);
    assert_eq!(cold.page_count(), 0);
    assert!(cold.cache.borrow().is_empty());
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
    assert!(
        file_len > HEADER_SIZE as u64,
        "push after clear reaches disk"
    );
    // The append maps nothing; the read above was positional.
    assert!(cold.mmap.is_none());
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
        assert!(file_len > HEADER_SIZE as u64);
    }

    // Reload so a mapping exists (load maps, appends do not); Drop must release it
    // so the file can be deleted (load-bearing on Windows).
    {
        let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
        assert!(
            cold.mmap.is_some(),
            "load must map the populated file before the drop under test"
        );
    }

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn disk_cold_append_leaves_map_extent_reads_still_work() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // One page, then reload so the store maps the whole file.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, line_count) = create_test_page(10, "Page0");
        cold.push_compressed(&compressed, line_count).unwrap();
    }
    let mut cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    let mapped_at_load = cold.mmap.as_ref().expect("load maps the file").len() as u64;
    let file_len_at_load = cold.file.as_ref().unwrap().metadata().unwrap().len();
    assert_eq!(mapped_at_load, file_len_at_load);

    // An append must not re-map: the file grows past the view, which keeps its
    // load-time length.
    let (compressed2, line_count2) = create_test_page(5, "Page1");
    cold.push_compressed(&compressed2, line_count2).unwrap();
    let file_len2 = cold.file.as_ref().unwrap().metadata().unwrap().len();
    assert!(file_len2 > mapped_at_load, "file should grow past the view");
    let mmap_len2 = cold.mmap.as_ref().expect("view survives appends").len() as u64;
    assert_eq!(mmap_len2, mapped_at_load, "append must not re-map");

    // The old page reads through the mapping, the new one positionally.
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

#[test]
fn disk_cold_reload_and_read() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

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
fn disk_cold_cache_size_zero_no_infinite_loop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // with_cache_size(0) clamps to 1.
    let config = DiskColdConfig::new(&path).with_cache_size(0);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(5, "Page0");
    cold.push_compressed(&compressed, line_count).unwrap();

    // Reading caches the page; this must not hang.
    let line = cold.get_line(0).expect("no error").expect("line present");
    assert_eq!(line.to_string(), "Page0-Line0");

    assert_eq!(cold.cache.borrow().len(), 1);
}

// DiskColdTier::truncate_front_lines.

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

/// truncate_front_lines removing all lines leaves DiskColdTier empty.
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

    let (c3, n3) = create_test_page(4, "P2");
    cold.push_compressed(&c3, n3).unwrap();
    assert_eq!(cold.line_count(), 7, "3 surviving + 4 new = 7");
    assert_eq!(cold.page_count(), 2, "1 surviving + 1 new = 2");

    // 3 lines from P1, then 4 from P2.
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

    // 4 pages of 3 lines.
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

/// Rotation oracle: interleave front truncation and appends across many page
/// boundaries and check every surviving line, driving the cursor + absolute-base
/// index through base advances, dead-prefix reclamation, compaction and the
/// drop-to-empty reset.
#[test]
fn disk_cold_rotation_interleave_matches_oracle() {
    fn push_page(
        cold: &mut DiskColdTier,
        oracle: &mut Vec<String>,
        next_page: &mut usize,
        lines: usize,
    ) {
        let prefix = format!("Pg{:03}", *next_page);
        *next_page += 1;
        let (c, n) = create_test_page(lines, &prefix);
        cold.push_compressed(&c, n).unwrap();
        for i in 0..lines {
            oracle.push(format!("{prefix}-Line{i}"));
        }
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("rotation-oracle.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Oracle: the flat list of lines the store must currently hold.
    let mut oracle: Vec<String> = Vec::new();
    let mut next_page = 0usize;

    // Phase 1: build 12 pages of varying sizes.
    for k in 0..12 {
        push_page(&mut cold, &mut oracle, &mut next_page, 3 + (k % 5));
    }

    // Phase 2: small truncations interleaved with appends: page drops,
    // partial-page offsets, pushes over a non-zero base, and compaction whenever
    // dead outgrows live.
    for k in 0..40 {
        let cut = (1 + (k % 7)).min(cold.line_count());
        cold.truncate_front_lines(cut);
        oracle.drain(..cut);
        assert_eq!(cold.line_count(), oracle.len(), "count after cut {k}");
        if k % 3 == 0 {
            push_page(&mut cold, &mut oracle, &mut next_page, 2 + (k % 6));
        }
        // A full sweep every fourth round still hits every base state.
        if k % 4 == 0 {
            for (i, want) in oracle.iter().enumerate() {
                let got = cold.get_line(i).unwrap().unwrap().to_string();
                assert_eq!(&got, want, "line {i} after round {k}");
            }
            assert!(cold.get_line(oracle.len()).unwrap().is_none());
        }
    }

    // Phase 3: drop to empty, then refill; the base resets with the index.
    let all = cold.line_count();
    cold.truncate_front_lines(all);
    oracle.clear();
    assert_eq!(cold.line_count(), 0);
    push_page(&mut cold, &mut oracle, &mut next_page, 5);
    for (i, want) in oracle.iter().enumerate() {
        let got = cold.get_line(i).unwrap().unwrap().to_string();
        assert_eq!(&got, want, "line {i} after refill");
    }

    // Phase 4: reload. Front truncation is logical until compaction, so a reload
    // may resurrect dropped pages, but the oracle lines are always its suffix.
    drop(cold);
    let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    let total = cold.line_count();
    assert!(
        total >= oracle.len(),
        "reload keeps at least the live lines"
    );
    for (j, want) in oracle.iter().enumerate() {
        let idx = total - oracle.len() + j;
        let got = cold.get_line(idx).unwrap().unwrap().to_string();
        assert_eq!(&got, want, "suffix line {j} after reload");
    }
}

// Crash recovery.

/// A crash mid-write leaves a complete page then a torn one; reload discards
/// the torn page and keeps the complete one.
#[test]
fn disk_cold_crash_recovery_partial_page_discarded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("crash-partial.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (compressed, count) = create_test_page(5, "Good");
        cold.push_compressed(&compressed, count).unwrap();
        cold.sync().unwrap();
    }

    // A page header claiming 1000 bytes, followed by only 10.
    {
        use std::io::{Seek, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::End(0)).unwrap();
        let fake_compressed_size: u32 = 1000;
        let fake_line_count: u32 = 10;
        file.write_all(&fake_compressed_size.to_le_bytes()).unwrap();
        file.write_all(&fake_line_count.to_le_bytes()).unwrap();
        file.write_all(&[0xAB; 10]).unwrap();
        file.flush().unwrap();
    }

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

/// A partial page header after the last complete page loads as zero pages.
#[test]
fn disk_cold_crash_recovery_truncated_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("crash-trunc-hdr.dtrm");

    {
        let config = DiskColdConfig::new(&path);
        let _cold = DiskColdTier::with_config(config).unwrap();
    }

    // Fewer than PAGE_HEADER_SIZE bytes: a crash at the start of a page write.
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&[0x01, 0x02, 0x03]).unwrap();
        file.flush().unwrap();
    }

    {
        let config = DiskColdConfig::new(&path);
        let cold = DiskColdTier::with_config(config).unwrap();
        assert_eq!(cold.page_count(), 0, "truncated header must be ignored");
        assert_eq!(cold.line_count(), 0);
    }
}

/// After an explicit sync the advisory header counters match the page data
/// (appends defer them; scan_pages never trusts them).
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

    let (header_pages, header_lines) = read_header_counts(&path);
    assert_eq!(header_pages, 2, "header page count must match");
    assert_eq!(header_lines, 8, "header line count must match (5+3)");

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(cold.page_count(), 2);
    assert_eq!(cold.line_count(), 8);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P0-Line0");
    assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "P1-Line0");
}

#[path = "disk_compaction_tests.rs"]
mod compaction;

/// `create()` syncs the header to disk at once (`File::flush` is a no-op).
#[test]
fn disk_cold_create_header_durable_on_disk() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let _cold = DiskColdTier::with_config(config).unwrap();

    // Read the on-disk header directly, bypassing the tier API.
    let (page_count, line_count) = read_header_counts(&path);
    assert_eq!(page_count, 0, "freshly created header should have 0 pages");
    assert_eq!(line_count, 0, "freshly created header should have 0 lines");
}

/// `clear()` syncs the zeroed header to disk at once (`File::flush` is a no-op).
#[test]
fn disk_cold_clear_header_durable_on_disk() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Appends defer the header catch-up, so sync() for a non-zero baseline.
    let (compressed, count) = create_test_page(10, "Page0");
    cold.push_compressed(&compressed, count).unwrap();
    cold.sync().unwrap();

    let (pages_before, lines_before) = read_header_counts(&path);
    assert_eq!(pages_before, 1, "should have 1 page before clear");
    assert_eq!(lines_before, 10, "should have 10 lines before clear");

    cold.clear().unwrap();

    // The on-disk header is zeroed now, not at Drop.
    let (pages_after, lines_after) = read_header_counts(&path);
    assert_eq!(pages_after, 0, "clear() must zero page count on disk");
    assert_eq!(lines_after, 0, "clear() must zero line count on disk");
}

/// After each push, line_count, page_count and the readable range agree.
#[test]
fn disk_cold_push_compressed_state_consistency() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("consistency.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let page_sizes = [3, 7, 1, 10, 5];
    let mut expected_total = 0;

    for (i, &size) in page_sizes.iter().enumerate() {
        let (compressed, count) = create_test_page(size, &format!("P{i}"));
        cold.push_compressed(&compressed, count).unwrap();
        expected_total += size;

        assert_eq!(
            cold.line_count(),
            expected_total,
            "line_count inconsistency after push {i}"
        );
        assert_eq!(
            cold.page_count(),
            i + 1,
            "page_count inconsistency after push {i}"
        );
        // Data from all pages stays readable.
        let first_line = cold.get_line(0).unwrap().unwrap();
        assert_eq!(
            first_line.to_string(),
            "P0-Line0",
            "first line must always be accessible"
        );
        let last_line = cold.get_line(expected_total - 1).unwrap().unwrap();
        assert_eq!(
            last_line.to_string(),
            format!("P{i}-Line{}", size - 1),
            "last line of most recent page must be accessible"
        );
    }
}

/// push_compressed with empty data or zero lines is a no-op.
#[test]
fn disk_cold_push_compressed_empty_is_noop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("noop.dtrm");

    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    // Push real data first.
    let (compressed, count) = create_test_page(5, "P0");
    cold.push_compressed(&compressed, count).unwrap();
    assert_eq!(cold.line_count(), 5);
    assert_eq!(cold.page_count(), 1);

    // Push empty compressed data — should be no-op.
    cold.push_compressed(&[], 10).unwrap();
    assert_eq!(
        cold.line_count(),
        5,
        "empty data push must not change state"
    );
    assert_eq!(cold.page_count(), 1);

    // Push with zero line count — should be no-op.
    cold.push_compressed(&compressed, 0).unwrap();
    assert_eq!(
        cold.line_count(),
        5,
        "zero line_count push must not change state"
    );
    assert_eq!(cold.page_count(), 1);
}

/// Front truncation across several pages in one call.
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

    // 27 lines: 5 full pages, then offset 2 into P5.
    cold.truncate_front_lines(27);
    assert_eq!((cold.line_count(), cold.page_count()), (13, 3));
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P5-Line2");
    assert_eq!(cold.get_line(12).unwrap().unwrap().to_string(), "P7-Line4");
    assert!(cold.get_line(13).unwrap().is_none());
}

/// `load_line` returns the right single line on a cache miss and on hits.
#[test]
fn disk_cold_get_line_cache_hit_single_extraction() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("cache-hit.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (c, n) = create_test_page(10, "Pg");
    cold.push_compressed(&c, n).unwrap();

    // A cache miss, then hits.
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "Pg-Line0");
    assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "Pg-Line5");
    assert_eq!(cold.get_line(9).unwrap().unwrap().to_string(), "Pg-Line9");
    assert!(cold.get_line(10).unwrap().is_none());
}

/// A `PageIndexEntry` whose range exceeds the mapped/file length returns `Err`,
/// never an out-of-bounds read.
#[test]
fn disk_cold_decompress_oob_offset_returns_err() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("oob-offset.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Pg");
    cold.push_compressed(&compressed, line_count).unwrap();
    // Reload so the mapped read path is the one under test.
    drop(cold);
    let mut cold =
        DiskColdTier::with_config(DiskColdConfig::new(dir.path().join("oob-offset.dtrm"))).unwrap();

    assert!(cold.decompress_page_for_test(0).is_ok());

    // Point the page range past the end of the mapped file.
    let map_len = cold.mmap.as_ref().expect("mmap present").len() as u64;
    cold.corrupt_last_entry_range(map_len + 1, 4096);

    let err = cold
        .decompress_page_for_test(0)
        .expect_err("out-of-bounds page range must error, not read OOB");
    assert!(
        matches!(err, ScrollbackError::Io(_)),
        "expected I/O error for OOB range, got: {err:?}"
    );
}

/// An in-bounds offset with a `compressed_size` past the mapping errors.
#[test]
fn disk_cold_decompress_oob_length_returns_err() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("oob-length.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Pg");
    cold.push_compressed(&compressed, line_count).unwrap();

    // Offset 32 (just after the file header) is valid, but a huge length
    // overruns the mapping.
    cold.corrupt_last_entry_range(HEADER_SIZE as u64, u32::MAX);

    let err = cold
        .decompress_page_for_test(0)
        .expect_err("oversized compressed_size must error, not read OOB");
    assert!(
        matches!(err, ScrollbackError::Io(_)),
        "expected I/O error for oversized length, got: {err:?}"
    );
}

/// Another process truncating the backing file under the mapping: the
/// live-length re-check returns `Err` instead of dereferencing past EOF.
#[test]
fn disk_cold_decompress_external_truncation_returns_err() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("truncated.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Pg");
    cold.push_compressed(&compressed, line_count).unwrap();
    // Reload so a mapping exists (load maps, appends do not).
    drop(cold);
    let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    assert!(cold.mmap.is_some(), "load must map the populated file");
    assert!(cold.decompress_page_for_test(0).is_ok());

    // Shrink the file to the header while the mapping keeps its original length.
    let truncator = std::fs::OpenOptions::new().write(true).open(&path).unwrap();

    #[cfg(unix)]
    {
        truncator.set_len(HEADER_SIZE as u64).unwrap();
        drop(truncator);

        let err = cold
            .decompress_page_for_test(0)
            .expect_err("read against a truncated file must error, not SIGBUS");
        assert!(
            matches!(err, ScrollbackError::Io(_)),
            "expected I/O error for truncated file, got: {err:?}"
        );
    }

    // Windows refuses to truncate a file with a live user-mapped section
    // (ERROR_USER_MAPPED_FILE, 1224), so the SIGBUS window does not exist there.
    #[cfg(windows)]
    {
        let err = truncator
            .set_len(HEADER_SIZE as u64)
            .expect_err("Windows must refuse to truncate a user-mapped file");
        assert_eq!(
            err.raw_os_error(),
            Some(1224),
            "expected ERROR_USER_MAPPED_FILE, got: {err:?}"
        );
        drop(truncator);
        assert!(
            cold.decompress_page_for_test(0).is_ok(),
            "mapping stays fully readable after the refused truncation"
        );
    }
}

/// When an external handle without FILE_SHARE_DELETE (antivirus or indexer,
/// simulated here) blocks compaction's rename, the tier stays operational
/// (reads work, appends reach the disk file) and the next compaction succeeds
/// once the blocker is gone.
#[cfg(windows)]
#[test]
fn disk_cold_compaction_survives_blocked_rename() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;

    let dir = tempdir().unwrap();
    let path = dir.path().join("compact-blocked.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();
    for i in 0..10 {
        let (c, n) = create_test_page(5, &format!("P{i}"));
        cold.push_compressed(&c, n).unwrap();
    }

    // Without FILE_SHARE_DELETE no rename can replace the name while this handle
    // lives, even with POSIX semantics.
    let blocker = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&path)
        .unwrap();

    // 6 of 10 pages dead: compaction fires and its rename fails against the
    // blocker; truncate_front_lines swallows the error.
    cold.truncate_front_lines(30);
    assert_eq!(cold.line_count(), 20);
    assert!(
        cold.is_disk_backed(),
        "tier must reopen the original file after a failed rename"
    );
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P6-Line0");
    assert_eq!(cold.get_line(19).unwrap().unwrap().to_string(), "P9-Line4");

    // Appends after the failed compaction still reach the disk file.
    let (c, n) = create_test_page(5, "P10");
    cold.push_compressed(&c, n).unwrap();
    assert_eq!(cold.get_line(24).unwrap().unwrap().to_string(), "P10-Line4");

    drop(blocker);
    let size_before = std::fs::metadata(&path).unwrap().len();
    cold.compact().unwrap();
    let size_after = std::fs::metadata(&path).unwrap().len();
    assert!(
        size_after < size_before,
        "retried compaction must reclaim dead space: before={size_before}, after={size_after}"
    );
    assert_eq!(cold.line_count(), 25);
    drop(cold);

    let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    assert_eq!(cold.line_count(), 25);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P6-Line0");
    assert_eq!(cold.get_line(24).unwrap().unwrap().to_string(), "P10-Line4");
}

// Deferred append durability.

/// Appends neither write the header nor fsync per page: the on-disk header stays
/// at (0, 0) until a barrier. The two tests below pin the durability side.
#[test]
fn disk_cold_append_defers_header_until_barrier() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("deferred-header.dtrm");
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(5, "P0");
        let (c2, n2) = create_test_page(3, "P1");
        cold.push_compressed(&c1, n1).unwrap();
        cold.push_compressed(&c2, n2).unwrap();

        // These small appends stay below the byte policy: counters still (0, 0).
        let (pages_live, lines_live) = read_header_counts(&path);
        assert_eq!(pages_live, 0, "no per-append header write");
        assert_eq!(lines_live, 0, "no per-append header write");

        // A freshly created store never maps; reads are positional.
        assert!(cold.mmap.is_none());
        assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P0-Line0");
        assert_eq!(cold.get_line(5).unwrap().unwrap().to_string(), "P1-Line0");
    }
    // Drop syncs the deferred header.
    let (pages, lines) = read_header_counts(&path);
    assert_eq!(pages, 2);
    assert_eq!(lines, 8);
}

/// Recovery does not depend on the deferred header: after a crash (no Drop, no
/// sync) scan_pages recovers every page whose bytes reached the file, though the
/// header still claims zero pages.
#[test]
fn disk_cold_crash_before_barrier_recovers_from_page_scan() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("crash-no-barrier.dtrm");
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(4, "P0");
        let (c2, n2) = create_test_page(6, "P1");
        cold.push_compressed(&c1, n1).unwrap();
        cold.push_compressed(&c2, n2).unwrap();
        // Crash: skip Drop (leaks the file handle for the rest of the process).
        std::mem::forget(cold);
    }
    let (pages_stale, _) = read_header_counts(&path);
    assert_eq!(
        pages_stale, 0,
        "header must be stale, else the crash simulation is void"
    );

    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(cold.page_count(), 2, "scan recovers both pages");
    assert_eq!(cold.line_count(), 10);
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P0-Line0");
    assert_eq!(cold.get_line(9).unwrap().unwrap().to_string(), "P1-Line5");
}

/// The byte-policy barrier fires during a burst: past `SYNC_APPEND_BYTES` of
/// compressed pages the on-disk header catches up with no close or sync call.
#[test]
fn disk_cold_append_barrier_fires_on_byte_policy() {
    // LCG noise: printable but incompressible, so compressed size ~= raw size.
    fn noise_page(lines: usize, len_per_line: usize, seed: &mut u64) -> (Vec<u8>, usize) {
        let make = |seed: &mut u64| {
            let mut s = String::with_capacity(len_per_line);
            while s.len() < len_per_line {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let b = 33u8 + ((*seed >> 33) % 94) as u8;
                s.push(b as char);
            }
            s
        };
        let lines_vec: Vec<Line> = (0..lines).map(|_| Line::from(&*make(seed))).collect();
        let serialized = serialize_lines(&lines_vec);
        let compressed = crate::encode_cold_block(&serialized).unwrap();
        (compressed, lines)
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("barrier-policy.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let mut seed = 0x5eed_5eed_5eed_5eed_u64;
    let mut caught_up = false;
    // ~256 KiB compressed per page: the 5th push crosses 1 MiB; 12 is margin.
    for attempt in 1..=12u64 {
        let (c, n) = noise_page(8, 32 * 1024, &mut seed);
        cold.push_compressed(&c, n).unwrap();
        let (pages_live, _) = read_header_counts(&path);
        if pages_live > 0 {
            assert!(
                pages_live <= attempt,
                "header can only reflect pushed pages"
            );
            caught_up = true;
            break;
        }
    }
    assert!(
        caught_up,
        "byte-policy barrier must fire during a sustained burst"
    );
}

/// An in-memory tier over pages of `line_counts` lines, built the way
/// `push_compressed` leaves the index (no file, so nothing is read).
fn tier_of_pages(line_counts: &[u32]) -> DiskColdTier {
    let mut cold = DiskColdTier::new();
    let mut total = 0usize;
    for &line_count in line_counts {
        cold.index.push(PageIndexEntry {
            offset: HEADER_SIZE as u64,
            compressed_size: 50,
            line_count,
        });
        total += len_u32_to_usize(line_count);
        cold.cumulative_lines.push(total);
    }
    cold.line_count = total;
    cold
}

/// Page lookup and front truncation over every small layout: 1..=4 pages of
/// 1..=6 lines (1554 layouts, every non-uniform cumulative boundary the `+1`
/// encoding meets). `find_page` brackets every line in its page, and after
/// `truncate_front_lines(n)` for every `n` the count drops by `n` and, while a
/// page survives, `front_offset` lies inside the first live page (which
/// `compact_page_trimmed`'s `&lines[trim..]` relies on).
#[test]
fn find_page_and_front_truncation_over_every_small_layout() {
    let mut layouts: Vec<Vec<u32>> = Vec::new();
    let mut frontier: Vec<Vec<u32>> = vec![Vec::new()];
    for _ in 0..4 {
        frontier = frontier
            .iter()
            .flat_map(|l| (1..=6).map(move |c| [l.as_slice(), &[c]].concat()))
            .collect();
        layouts.extend(frontier.iter().cloned());
    }
    assert_eq!(layouts.len(), 6 + 36 + 216 + 1296);
    for layout in &layouts {
        let cold = tier_of_pages(layout);
        let total = cold.line_count;
        for line in 0..total {
            let page = cold
                .find_page(line)
                .unwrap_or_else(|| panic!("{layout:?}: no page for line {line}"));
            let start = page.checked_sub(1).map_or(0, |p| cold.cumulative_lines[p]);
            assert!(
                start <= line && line < cold.cumulative_lines[page],
                "{layout:?}: line {line} in page {page}"
            );
        }
        assert_eq!(cold.find_page(total), None, "{layout:?}: past the end");
        for n in 1..=total {
            let mut cold = tier_of_pages(layout);
            cold.truncate_front_lines(n);
            assert_eq!(cold.line_count, total - n, "{layout:?} minus {n}");
            if let Some(first) = cold.live_index().first() {
                assert!(
                    cold.front_offset < len_u32_to_usize(first.line_count),
                    "{layout:?} minus {n}: front_offset {} past the first live page",
                    cold.front_offset
                );
            }
        }
    }
}
