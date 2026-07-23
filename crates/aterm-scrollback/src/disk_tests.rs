// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for `DiskColdTier` — extracted from `disk.rs` (#2100).

use super::super::line::serialize_lines;
use super::*;
use crate::ScrollbackError;
use aterm_tempfile::tempdir;
use std::fs::File;
use std::io::Read;

impl DiskColdTier {
    /// Get the number of pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.index.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Check if disk-backed.
    #[must_use]
    pub fn is_disk_backed(&self) -> bool {
        self.file.is_some()
    }

    /// Test-only: corrupt the last index entry so it points past the mapped
    /// region, simulating a malformed/attacker-influenced `PageIndexEntry` or
    /// an out-of-band file truncation.
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
    let compressed = zstd::encode_all(serialized.as_slice(), 3).unwrap();
    (compressed, line_count)
}

#[test]
fn disk_cold_cache_byte_limit_is_enforced() {
    // Round-10: the cold LRU cache used to evict by page COUNT only, ignoring the
    // documented (and Kani-"proven") cache_byte_limit. cache_size here is large so
    // ONLY the byte budget can bind; reading more distinct pages than the budget
    // holds must keep cache_bytes <= cache_byte_limit at all times.
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
    // With total cached-page bytes far over the budget and a large page-count cap,
    // the byte budget is the binding constraint, so not every page can be resident.
    assert!(
        cold.cache.borrow().len() < num_pages,
        "the byte budget must force eviction below the page-count cap"
    );
    // cache_bytes stays exactly the sum of per-entry byte sizes (Kani post-cond 3).
    let sum: usize = cold.cache.borrow().values().map(|e| e.bytes).sum();
    assert_eq!(
        cold.cache_bytes.get(),
        sum,
        "cache_bytes must equal the sum of entry sizes"
    );
}

#[test]
fn disk_cold_skips_caching_an_oversized_page() {
    // A single page whose decompressed footprint exceeds the whole byte budget is
    // uncacheable — it must not be inserted (a re-read re-decompresses).
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

/// REGRESSION: a corrupt / torn / tampered `page_count` header field must NOT
/// abort the reload with a `Vec::with_capacity` capacity-overflow panic.
/// `page_count` is only a preallocation hint — `scan_pages` rebuilds the index
/// from the page headers — so a bogus count must degrade gracefully to the real
/// page data rather than crashing scrollback restore.
#[test]
fn corrupt_page_count_header_does_not_panic_on_reload() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold.dtrm");

    // Write two real pages, then drop the tier so the file is flushed/closed.
    {
        let config = DiskColdConfig::new(&path);
        let mut cold = DiskColdTier::with_config(config).unwrap();
        let (c1, n1) = create_test_page(3, "A");
        cold.push_compressed(&c1, n1).unwrap();
        let (c2, n2) = create_test_page(4, "B");
        cold.push_compressed(&c2, n2).unwrap();
        cold.sync().unwrap();
    }

    // Corrupt ONLY the 8-byte page_count field (header[8..16]) to u64::MAX.
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(8)).unwrap();
        f.write_all(&u64::MAX.to_le_bytes()).unwrap();
        f.flush().unwrap();
    }

    // Reload must NOT panic; the page-header scan recovers both real pages
    // regardless of the bogus count.
    let config = DiskColdConfig::new(&path);
    let cold = DiskColdTier::with_config(config).unwrap();
    assert_eq!(
        cold.page_count(),
        2,
        "both real pages must be recovered from the page-header scan despite a corrupt page_count"
    );
}

/// REGRESSION (round 15): `scan_pages` preallocated its index / cumulative_lines
/// Vecs from `page_count.min(max_pages)`, where `max_pages` = (file_len − 32)/8 is
/// proportional to the file's LOGICAL length — cheaply attacker-controllable via a
/// sparse file. A crafted .dtrm (valid header, page_count = u64::MAX) truncated to
/// a large sparse size made both `Vec::with_capacity` calls eagerly reserve ~3× the
/// logical length (16 B PageIndexEntry + 8 B usize per slot) BEFORE the scan
/// discovered the first sparse (zero) page and stopped — an OOM-abort of scrollback
/// restore on memory-constrained systems. The MAX_PREALLOC_PAGES cap bounds the
/// initial reservation to a fixed size; the scan still recovers the real pages.
///
/// Uses a 1 GiB sparse tail: max_pages ≈ 1.34e8, so the cap (1<<20) clamps the
/// reservation from ~3.2 GB down to ~25 MB. The sparse region is all zeros, so the
/// scan hits a compressed_size==0 page right after the two real ones and stops —
/// only the transient prealloc was dangerous. The file is sparse (ftruncate) and
/// the whole-file mmap is lazy/file-backed, so nothing here touches real memory.
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

    // Corrupt page_count to u64::MAX AND extend the file to a 1 GiB sparse tail.
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(8)).unwrap();
        f.write_all(&u64::MAX.to_le_bytes()).unwrap();
        f.set_len(1024 * 1024 * 1024).unwrap(); // 1 GiB sparse
        f.flush().unwrap();
    }

    // Reload must NOT OOM-abort; the scan recovers exactly the two real pages.
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

/// Backward-compat (codex round-5): a legacy `.dtrm` page can declare more lines
/// than the current decoder reconstructs (`MAX_DECODE_PAGE_LINES`) if it was
/// written by an older, larger-`block_size` build. `scan_pages` must CLAMP such a
/// page's recorded `line_count` on load so `cumulative_lines`/`get_line` never
/// address the undecodable suffix (accepted-but-unreadable history). A page this
/// build writes can never exceed the cap (block_size is clamped to it).
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
    // Reopen → scan_pages clamps the recorded line_count to the decode cap.
    let cold = DiskColdTier::with_config(DiskColdConfig::new(&path)).unwrap();
    assert_eq!(
        cold.line_count(),
        crate::line::MAX_DECODE_PAGE_LINES,
        "an oversized legacy page's line_count is clamped to the decode cap on load"
    );
}

/// Regression (round 11): `scan_pages` must stop at a page whose `line_count`
/// is 0. Such a page advances `cumulative` by 0, so its `cumulative_lines`
/// entry duplicates the previous total, breaking the strictly-increasing
/// invariant that `find_page`'s binary search relies on — a later lookup could
/// resolve a valid line to the empty page and mis-slice it. A zero-line page
/// (with nonzero compressed_size, so the length-0 terminator does not fire)
/// only appears via truncation/corruption, so treat it as end-of-data.
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

    // Append a corrupt page: 8-byte header {compressed_size = 4, line_count = 0}
    // followed by 4 bytes of payload so the page_end bounds check would pass and
    // scan_pages reaches the line_count guard (checked first).
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

    // Every real line remains readable — cumulative_lines stayed strictly
    // increasing, so the binary search still lands on the right page.
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
    let mmap_len = cold
        .mmap
        .as_ref()
        .expect("mmap must be re-established after clear+push")
        .len() as u64;
    assert_eq!(mmap_len, file_len);
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

/// Verify push_compressed maintains transactional state consistency (#7575).
///
/// After each push, line_count, page_count, and cumulative_lines must all
/// agree. This test exercises the transactional commit pattern that was
/// introduced to prevent inconsistent state on I/O failure.
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

        // Invariant: line_count must equal the sum of all pushed line counts.
        assert_eq!(
            cold.line_count(),
            expected_total,
            "line_count inconsistency after push {i}"
        );
        // Invariant: page_count must match the number of pushes.
        assert_eq!(
            cold.page_count(),
            i + 1,
            "page_count inconsistency after push {i}"
        );
        // Invariant: data from all pages must be readable.
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

/// push_compressed with empty data or zero lines is a no-op (#7575).
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

/// Regression: a malformed `PageIndexEntry` whose `offset + compressed_size`
/// exceeds the mapped/file length must return an `Err`, never read out of
/// bounds (which would be a SIGBUS / OOB read against the raw mmap pointer).
#[test]
fn disk_cold_decompress_oob_offset_returns_err() {
    let dir = tempdir().unwrap();
    let config = DiskColdConfig::new(dir.path().join("oob-offset.dtrm"));
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Pg");
    cold.push_compressed(&compressed, line_count).unwrap();

    // Valid read works before corruption.
    assert!(cold.decompress_page_for_test(0).is_ok());

    // Push the page range far past the end of the mapped file.
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

/// Regression: an entry whose offset stays in-bounds but whose
/// `compressed_size` runs past the mapped length must error rather than
/// slicing past the mapping.
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

/// Regression: simulate another process truncating the backing file after the
/// mapping was created. The live-file-length re-check must catch the shrink
/// and return an `Err` instead of dereferencing past EOF (SIGBUS).
#[test]
fn disk_cold_decompress_external_truncation_returns_err() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("truncated.dtrm");
    let config = DiskColdConfig::new(&path);
    let mut cold = DiskColdTier::with_config(config).unwrap();

    let (compressed, line_count) = create_test_page(10, "Pg");
    cold.push_compressed(&compressed, line_count).unwrap();
    assert!(cold.decompress_page_for_test(0).is_ok());

    // Simulate an out-of-band truncation by another process: shrink the file
    // to just the header while the mapping still records the original length.
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

    // On Windows the OS itself enforces the invariant this regression guards:
    // a file with a live user-mapped section cannot be truncated by ANY
    // process (ERROR_USER_MAPPED_FILE, 1224), so the SIGBUS window does not
    // exist. Assert that stronger guarantee instead.
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

/// Windows regression: compaction closes its own destination handle before the
/// rename (required off-NTFS, where POSIX-semantics replace is unavailable).
/// When an external handle without FILE_SHARE_DELETE (antivirus/indexer,
/// simulated here) still blocks the rename, the failed compaction must leave
/// the tier fully operational — reads keep working, later appends still reach
/// the disk file — and the next compaction succeeds once the blocker is gone.
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

    // Hold the destination open WITHOUT FILE_SHARE_DELETE: no rename can
    // replace the name while this handle lives, even with POSIX semantics.
    let blocker = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&path)
        .unwrap();

    // 6 of 10 pages dead (>50%) → compaction fires; its rename fails against
    // the blocker. truncate_front_lines swallows the error by contract.
    cold.truncate_front_lines(30);
    assert_eq!(cold.line_count(), 20);
    assert!(
        cold.is_disk_backed(),
        "tier must reopen the original file after a failed rename"
    );
    assert_eq!(cold.get_line(0).unwrap().unwrap().to_string(), "P6-Line0");
    assert_eq!(cold.get_line(19).unwrap().unwrap().to_string(), "P9-Line4");

    // Appends after the failed compaction must land in the disk file, not
    // silently degrade to memory-only mode.
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
