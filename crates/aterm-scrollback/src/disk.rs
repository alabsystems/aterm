// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Disk-backed cold tier storage using memory-mapped files.
//!
//! File format defined in [`disk_format`](super::disk_format).
//! Pages are loaded on demand and cached in an LRU cache for repeated access.
//! The index is rebuilt on load by scanning page headers.

pub use super::disk_format::DiskColdConfig;
use super::disk_format::{
    DEFAULT_CACHE_BYTE_LIMIT, DEFAULT_CACHE_SIZE, HEADER_SIZE, MAGIC, PAGE_HEADER_SIZE,
    PageIndexEntry, VERSION, len_to_u32, len_u32_to_usize, len_u64_to_usize,
};
use super::line::{Line, MAX_DECODE_PAGE_LINES};
use crate::mmap::MmapMut;
use std::cell::Cell;
use std::cell::RefCell;
#[cfg(not(kani))]
use std::collections::HashMap;
// Under Kani, BTreeMap avoids unsupported CCRandomGenerateBytes FFI from HashMap's hasher seed.
#[cfg(kani)]
use std::collections::BTreeMap as HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

struct CacheEntry {
    lines: Vec<Line>,
    last_access: u64,
    /// Byte cost of this cached page (== the amount added to `cache_bytes` on
    /// insert), stored so eviction can subtract it exactly and keep the running
    /// `cache_bytes` sum consistent (Kani `cache_byte_limit_respected`, post-cond 3).
    bytes: usize,
}

/// Disk-backed cold tier storage.
///
/// Stores Zstd-compressed pages in a memory-mapped file with lazy loading.
///
/// The LRU page cache uses interior mutability (`RefCell`/`Cell`) so that
/// `get_line` can take `&self`. This mirrors the pattern used by
/// [`ColdTier`](super::ColdTier) and allows FFI functions that only read
/// terminal state to accept `*const AtermTerminal`.
#[derive(Debug)]
pub struct DiskColdTier {
    /// Storage file.
    file: Option<File>,
    /// Memory map of the file (for reading).
    mmap: Option<MmapMut>,
    /// Path to the storage file.
    path: PathBuf,
    /// Page index (kept in memory for fast lookup).
    index: Vec<PageIndexEntry>,
    /// Total line count.
    line_count: usize,
    /// Cumulative line counts for binary search.
    cumulative_lines: Vec<usize>,
    /// LRU cache of decompressed pages (interior mutability for `&self` reads).
    cache: RefCell<HashMap<usize, CacheEntry>>,
    /// Cache size limit (max number of cached decompressed pages).
    cache_size: usize,
    /// Byte budget for decompressed cached pages. Enforced by `cache_page`
    /// alongside `cache_size` so a malicious `.dtrm` with a few huge (up to the
    /// 64 MiB per-page decompression cap) pages cannot pin `cache_size × 64 MiB`
    /// of decompressed lines resident — the cold read cache is deliberately
    /// excluded from the tier memory budget, so this is its only byte bound. The
    /// Kani proof `cache_byte_limit_respected` models this eviction.
    cache_byte_limit: usize,
    /// Running total of cached decompressed-page bytes; maintained ==
    /// Σ `CacheEntry::bytes` at every cache mutation (insert/evict/clear).
    cache_bytes: Cell<usize>,
    /// Access counter for LRU (interior mutability for `&self` reads).
    access_counter: Cell<u64>,
    /// Next write offset in file.
    write_offset: u64,
    /// Running total for `memory_used()`.
    bytes_used: Cell<usize>,
    /// Lines logically consumed from the first page. Avoids decompression
    /// during line-limit truncation — pages are dropped when fully consumed.
    front_offset: usize,
    /// Compressed bytes appended since the last append-side durability
    /// barrier (`SYNC_APPEND_BYTES` in disk_write.rs). Drives the deferred
    /// fsync policy only; never consulted by reads or indexing.
    unsynced_append_bytes: usize,
    /// True when the on-disk header's page/line counters lag the in-memory
    /// index. The counters are ADVISORY (`scan_pages` rebuilds the index from
    /// page headers and only uses them as a clamped preallocation hint), so
    /// lag is always safe — this flag merely schedules the catch-up write at
    /// the next barrier.
    header_dirty: bool,
    /// Shared dead-prefix length of `index` AND `cumulative_lines` (their
    /// first `front_dropped` entries belong to dropped pages). Front drops
    /// advance this cursor in O(dropped) instead of draining both vectors —
    /// the old path memmoved `index` and memmoved+rebased `cumulative_lines`,
    /// O(total pages) per drop, on the line-limit path of every push. The
    /// prefix is reclaimed by one amortized memmove when it outgrows the
    /// live half (see `drop_front_index_entries`).
    front_dropped: usize,
    /// Absolute cumulative value at the current front: total physical lines
    /// in pages dropped since the last full rebuild. Live `cumulative_lines`
    /// entries are ABSOLUTE (base + physical lines in live pages `0..=i`);
    /// lookups add the base to the search target instead of ever rebasing
    /// stored values. Saturating bumps are exact on every real path (crate
    /// idiom).
    cumulative_base: usize,
}

impl DiskColdTier {
    /// Create a new in-memory cold tier (no disk backing).
    #[must_use]
    pub fn new() -> Self {
        Self {
            file: None,
            mmap: None,
            path: PathBuf::new(),
            index: Vec::new(),
            line_count: 0,
            cumulative_lines: Vec::new(),
            cache: RefCell::new(HashMap::new()),
            cache_size: DEFAULT_CACHE_SIZE,
            cache_byte_limit: DEFAULT_CACHE_BYTE_LIMIT,
            cache_bytes: Cell::new(0),
            access_counter: Cell::new(0),
            write_offset: HEADER_SIZE as u64,
            bytes_used: Cell::new(0),
            front_offset: 0,
            unsynced_append_bytes: 0,
            header_dirty: false,
            front_dropped: 0,
            cumulative_base: 0,
        }
        .with_computed_bytes_used()
    }

    /// Create a disk-backed cold tier; loads existing file or creates new.
    ///
    /// Cleans up orphan `.dtrm.tmp` files left by a crash during compaction
    /// before opening the main store (#5964).
    pub fn with_config(config: DiskColdConfig) -> io::Result<Self> {
        let path = config.path;
        let cache_size = config.cache_size;
        // Clamp to >=1 so a zero byte limit does not silently disable caching
        // (DiskColdConfig::new defaults it to DEFAULT_CACHE_BYTE_LIMIT).
        let cache_byte_limit = config.cache_byte_limit.max(1);

        // Remove orphan compaction temp files if present. A crash between
        // `File::create(tmp)` and `fs::rename(tmp, main)` in `compact()`
        // leaves incomplete temp files that must be discarded.
        let tmp_path = path.with_extension("dtrm.tmp");
        if tmp_path.exists() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        // Also clean up `.dtrm.compact` orphans from the compaction output path.
        let compact_path = path.with_extension("dtrm.compact");
        if compact_path.exists() {
            let _ = std::fs::remove_file(&compact_path);
        }

        if path.exists() {
            Self::load(&path, cache_size, cache_byte_limit)
        } else {
            Self::create(&path, cache_size, cache_byte_limit)
        }
    }

    fn create(path: &Path, cache_size: usize, cache_byte_limit: usize) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            crate::storage::create_dir_restricted(parent)?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        // Write header
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        // page_count and line_count start at 0
        file.write_all(&header)?;
        file.sync_data()?;

        Ok(Self {
            file: Some(file),
            mmap: None,
            path: path.to_path_buf(),
            index: Vec::new(),
            line_count: 0,
            cumulative_lines: Vec::new(),
            cache: RefCell::new(HashMap::new()),
            cache_size,
            cache_byte_limit,
            cache_bytes: Cell::new(0),
            access_counter: Cell::new(0),
            write_offset: HEADER_SIZE as u64,
            bytes_used: Cell::new(0),
            front_offset: 0,
            unsynced_append_bytes: 0,
            header_dirty: false,
            front_dropped: 0,
            cumulative_base: 0,
        }
        .with_computed_bytes_used())
    }

    /// Load an existing storage file.
    fn load(path: &Path, cache_size: usize, cache_byte_limit: usize) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let page_count = Self::validate_header(&mut file)?;
        let file_len = file.metadata()?.len();
        let (index, cumulative_lines, line_count, write_offset) =
            Self::scan_pages(&mut file, file_len, page_count)?;

        // SAFETY: File is exclusively owned; no external process modifies it.
        let mmap = if file_len > HEADER_SIZE as u64 {
            Some(unsafe { MmapMut::map_mut(&file)? })
        } else {
            None
        };

        Ok(Self {
            file: Some(file),
            mmap,
            path: path.to_path_buf(),
            index,
            line_count,
            cumulative_lines,
            cache: RefCell::new(HashMap::new()),
            cache_size,
            cache_byte_limit,
            cache_bytes: Cell::new(0),
            access_counter: Cell::new(0),
            write_offset,
            bytes_used: Cell::new(0),
            front_offset: 0,
            unsynced_append_bytes: 0,
            header_dirty: false,
            front_dropped: 0,
            cumulative_base: 0,
        }
        .with_computed_bytes_used())
    }

    /// Validate file header; returns page count on success.
    fn validate_header(file: &mut File) -> io::Result<usize> {
        let mut header = [0u8; HEADER_SIZE];
        file.read_exact(&mut header)?;
        if &header[0..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid magic bytes",
            ));
        }
        let version = u32::from_le_bytes(
            header[4..8]
                .try_into()
                .expect("invariant: 4-byte slice fits [u8; 4]"),
        );
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported version: {version}"),
            ));
        }
        len_u64_to_usize(u64::from_le_bytes(
            header[8..16]
                .try_into()
                .expect("invariant: 8-byte slice fits [u8; 8]"),
        ))
    }

    /// Scan page headers to rebuild the in-memory index.
    fn scan_pages(
        file: &mut File,
        file_len: u64,
        capacity: usize,
    ) -> io::Result<(Vec<PageIndexEntry>, Vec<usize>, usize, u64)> {
        // `capacity` is the `page_count` field read verbatim from the on-disk
        // header; it is ONLY a preallocation hint (the index is rebuilt by
        // scanning page headers below, never trusted from this field). Clamp it
        // to the most pages the file could physically hold — every page occupies
        // at least PAGE_HEADER_SIZE bytes — so a corrupt / torn / tampered
        // page_count (e.g. u64::MAX) cannot drive `Vec::with_capacity` past
        // isize::MAX and abort scrollback restore with a capacity-overflow panic
        // instead of letting the corruption-tolerant scan below gracefully
        // discard the bad tail. `try_from` (not `as usize`) avoids a silent
        // truncation of the page-count cap on 32-bit targets.
        let max_pages =
            usize::try_from(file_len.saturating_sub(HEADER_SIZE as u64) / PAGE_HEADER_SIZE as u64)
                .unwrap_or(usize::MAX);
        // `max_pages` only guards the isize::MAX capacity-overflow panic; it is
        // proportional to the file's LOGICAL length, which is attacker-controllable
        // cheaply via a sparse file. A crafted .dtrm (valid 32-byte header,
        // page_count = u64::MAX, ftruncate'd to e.g. 100 GiB of holes) would make
        // both Vec::with_capacity calls below eagerly reserve ~3x the logical length
        // (16 B PageIndexEntry + 8 B usize per slot) BEFORE the scan discovers the
        // first sparse page (compressed_size == 0) and stops — enough to abort
        // restore via handle_alloc_error. Bound the initial reservation to a fixed
        // absolute cap, mirroring deserialize_lines' MAX_PREALLOC_LINES. A page is
        // one warm block, so even a legitimate 100 GiB cold store is ~1e6 pages;
        // 1<<20 comfortably exceeds that. The Vecs still grow from genuine page
        // pushes below, so real large stores lose nothing — the cap only bites on a
        // lying/sparse header.
        const MAX_PREALLOC_PAGES: usize = 1 << 20;
        let capacity = capacity.min(max_pages).min(MAX_PREALLOC_PAGES);
        let mut index = Vec::with_capacity(capacity);
        let mut cumulative_lines = Vec::with_capacity(capacity);
        let mut cumulative = 0usize;
        let mut offset = HEADER_SIZE as u64;
        let mut buf = [0u8; PAGE_HEADER_SIZE];

        while offset + PAGE_HEADER_SIZE as u64 <= file_len {
            file.seek(SeekFrom::Start(offset))?;
            if file.read_exact(&mut buf).is_err() {
                break;
            }
            let compressed_size = u32::from_le_bytes(
                buf[0..4]
                    .try_into()
                    .expect("invariant: 4-byte slice fits [u8; 4]"),
            );
            let line_count = u32::from_le_bytes(
                buf[4..8]
                    .try_into()
                    .expect("invariant: 4-byte slice fits [u8; 4]"),
            );
            // A zero-length page marks end-of-data. A zero-line page is equally
            // degenerate: it advances `cumulative` by 0, so its cumulative_lines
            // entry duplicates the previous one and breaks the strictly-increasing
            // invariant that find_page's binary search relies on (it could resolve
            // a line to the empty page and mis-slice it). Real pages always carry
            // >= 1 line, so treat line_count == 0 as corruption/truncation and stop.
            if compressed_size == 0 || line_count == 0 {
                break;
            }
            // Validate page data fits within file (crash recovery: #5917).
            // A crash mid-write leaves a partial page at the end — discard it.
            let page_end = offset
                .saturating_add(PAGE_HEADER_SIZE as u64)
                .saturating_add(u64::from(compressed_size));
            if page_end > file_len {
                break;
            }
            // Clamp a page's recorded line_count to what the page decoder will
            // actually reconstruct (`deserialize_page_lines` caps at
            // MAX_DECODE_PAGE_LINES). A page THIS build wrote can never exceed the
            // cap (block_size is clamped to the same value), but a legacy `.dtrm`
            // from an older, larger-`block_size` config could record more lines than
            // are decodable. Without this clamp `cumulative_lines` would address the
            // undecodable suffix and `get_line` would fail (accepted-but-unreadable
            // history); clamping keeps the first MAX_DECODE_PAGE_LINES lines readable
            // and honestly drops the rest. `offset` advances by byte size, so later
            // pages still load.
            let line_count = if len_u32_to_usize(line_count) > MAX_DECODE_PAGE_LINES {
                aterm_log::warn!(
                    "scan_pages: page line_count {line_count} exceeds MAX_DECODE_PAGE_LINES \
                     ({MAX_DECODE_PAGE_LINES}); clamping (legacy oversized page, suffix dropped)"
                );
                len_to_u32(MAX_DECODE_PAGE_LINES)
            } else {
                line_count
            };
            index.push(PageIndexEntry {
                offset,
                compressed_size,
                line_count,
            });
            cumulative += len_u32_to_usize(line_count);
            cumulative_lines.push(cumulative);
            offset = offset
                .saturating_add(PAGE_HEADER_SIZE as u64)
                .saturating_add(u64::from(compressed_size));
        }

        #[cfg(debug_assertions)]
        {
            let total: usize = index.iter().map(|e| e.line_count as usize).sum();
            debug_assert_eq!(cumulative, total, "line count matches index");
        }

        // Release any prealloc over-reservation once the true page count is known
        // (parity with deserialize_lines): a header that lied high about page_count
        // is now scanned down to reality, so drop the slack back to the OS.
        index.shrink_to_fit();
        cumulative_lines.shrink_to_fit();

        Ok((index, cumulative_lines, cumulative, offset))
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the total compressed size on disk (live pages only).
    #[must_use]
    pub fn compressed_size(&self) -> usize {
        self.live_index()
            .iter()
            .map(|e| len_u32_to_usize(e.compressed_size))
            .sum()
    }

    /// Estimate in-memory usage (bytes). Excludes mmap pages.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        self.bytes_used.get()
    }

    /// Live (non-dropped) page-index entries. Total `get` keeps it
    /// panic-free; `front_dropped <= len` is a maintained invariant.
    #[inline]
    pub(super) fn live_index(&self) -> &[PageIndexEntry] {
        self.index.get(self.front_dropped..).unwrap_or(&[])
    }

    /// Live region of the cumulative index (parallel to [`Self::live_index`]).
    #[inline]
    pub(super) fn live_cumulative(&self) -> &[usize] {
        self.cumulative_lines.get(self.front_dropped..).unwrap_or(&[])
    }

    /// Drop the first `k` LIVE pages from the index in O(k) amortized:
    /// advance the shared cursor and the absolute base, leaving every
    /// surviving entry of BOTH vectors untouched. The dead prefix is
    /// reclaimed by a single memmove of both vectors only once it outgrows
    /// the live half — amortized O(1) per dropped page, single-call bound
    /// O(live) word-moves — replacing the old unconditional drain-and-rebase
    /// (O(total pages) memmove + rewrite per drop). Clears everything (and
    /// resets the base) when no live page survives, so `empty => base == 0`
    /// holds for `push_compressed`.
    // Skip: `Vec::drain` under its guard — the BLANKET-unmodeled drain class
    // (guards don't chain). Same audit as `truncate_front_lines`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn drop_front_index_entries(&mut self, k: usize) {
        if k == 0 {
            return;
        }
        let live_len = self.live_index().len();
        if k >= live_len {
            self.index.clear();
            self.cumulative_lines.clear();
            self.front_dropped = 0;
            self.cumulative_base = 0;
            return;
        }
        // New base = absolute value of the LAST dropped entry. `get` keeps
        // the lookup total; the None arm is unreachable (1 <= k < live_len
        // just established, so `front_dropped + k - 1 < len`) and falls back
        // to the current base — it cannot execute on any real path.
        let last_dropped = self.front_dropped.saturating_add(k).saturating_sub(1);
        self.cumulative_base = self
            .cumulative_lines
            .get(last_dropped)
            .copied()
            .unwrap_or(self.cumulative_base);
        self.front_dropped = self.front_dropped.saturating_add(k);
        // Amortized reclamation of the dead prefix (both vectors together —
        // they are parallel by construction).
        if self.front_dropped > self.index.len().saturating_sub(self.front_dropped) {
            let dead = self.front_dropped;
            self.index.drain(..dead);
            self.cumulative_lines.drain(..dead);
            self.front_dropped = 0;
        }
    }

    /// Bytes of dead (unreclaimable) space at the front of the file.
    ///
    /// After `truncate_front_lines` drops pages, the space between the file
    /// header and the first surviving page is dead — it cannot be reclaimed
    /// by `ftruncate` (which only trims from the end).
    fn dead_bytes(&self) -> u64 {
        self.live_index()
            .first()
            .map_or(0, |e| e.offset.saturating_sub(HEADER_SIZE as u64))
    }

    /// Bytes of live compressed data in the file (surviving pages).
    fn live_bytes(&self) -> u64 {
        self.write_offset
            .saturating_sub(self.dead_bytes())
            .saturating_sub(HEADER_SIZE as u64)
    }

    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub(crate) fn recompute_memory_used(&self) -> usize {
        self.calculate_memory_used()
    }
}

/// Flush and unmap before closing the backing file.
impl Drop for DiskColdTier {
    fn drop(&mut self) {
        // Deferred append durability: catch the header up and fsync before
        // closing, so a clean exit loses nothing despite the batched
        // barriers (power loss mid-session is the only widened window).
        let _ = self.sync_appends();
        let _ = self.flush_and_drop_mmap();
        if let Some(ref mut file) = self.file {
            let _ = file.sync_all();
        }
    }
}

impl Default for DiskColdTier {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheEntry")
            .field("lines_count", &self.lines.len())
            .field("last_access", &self.last_access)
            .finish()
    }
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;

#[path = "disk_memory.rs"]
mod memory;

#[path = "disk_write.rs"]
mod write;

#[path = "disk_read.rs"]
mod read;

#[path = "disk_front_truncation.rs"]
mod front_truncation;

#[path = "disk_compaction.rs"]
mod compaction;

#[cfg(kani)]
#[path = "disk_kani.rs"]
mod proofs;
