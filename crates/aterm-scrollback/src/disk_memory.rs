// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory accounting, page decompression, the LRU page cache, and newest-end
//! (back) removal for [`DiskColdTier`].

use super::{CacheEntry, DiskColdTier, HashMap, Line, PageIndexEntry};
use crate::disk_format::{HEADER_SIZE, PAGE_HEADER_SIZE, len_to_u32, len_u32_to_usize};
use crate::line::deserialize_page_lines;
use crate::mmap::MmapMut;
use std::io::{self, Seek, SeekFrom, Write};

impl DiskColdTier {
    pub(crate) fn with_computed_bytes_used(self) -> Self {
        self.reset_bytes_used();
        self
    }

    pub(crate) fn reset_bytes_used(&self) {
        self.bytes_used.set(self.calculate_memory_used());
    }

    pub(crate) fn calculate_memory_used(&self) -> usize {
        // Saturating throughout: a pathological capacity clamps to `usize::MAX`
        // rather than panic or wrap to a bogus small reading, and the trust lane
        // proves each operation panic-free.
        let base = std::mem::size_of::<Self>();
        let path_mem = self.path.capacity();
        let index_mem = self
            .index
            .capacity()
            .saturating_mul(std::mem::size_of::<PageIndexEntry>());
        let cumulative_mem = self
            .cumulative_lines
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>());
        let cache = self.cache.borrow();
        let cache_struct_mem = Self::cache_bucket_count(&cache).saturating_mul(
            std::mem::size_of::<usize>().saturating_add(std::mem::size_of::<CacheEntry>()),
        );
        let line_struct_size = std::mem::size_of::<Line>();
        let cache_lines_mem: usize = cache
            .values()
            .map(|entry| {
                let lines_mem = entry.lines.capacity().saturating_mul(line_struct_size);
                let contents_mem: usize = entry
                    .lines
                    .iter()
                    .map(|line| line.memory_used().saturating_sub(line_struct_size))
                    .fold(0usize, usize::saturating_add);
                lines_mem.saturating_add(contents_mem)
            })
            .fold(0usize, usize::saturating_add);
        base.saturating_add(path_mem)
            .saturating_add(index_mem)
            .saturating_add(cumulative_mem)
            .saturating_add(cache_struct_mem)
            .saturating_add(cache_lines_mem)
    }

    fn cache_bucket_count(cache: &HashMap<usize, CacheEntry>) -> usize {
        cache.capacity()
    }

    /// Decompress a page from the mapped view when it covers the page, else by
    /// positional read ([`Self::read_page_bytes`]).
    ///
    /// Appends do not re-map (disk_write.rs), so the view's extent dates from load
    /// or compaction and the newest pages normally take the positional path; the
    /// map serves the older pages it covers with zero syscalls.
    pub(super) fn decompress_page(
        &self,
        page_idx: usize,
    ) -> Result<Vec<Line>, crate::ScrollbackError> {
        let Some(entry) = self.live_index().get(page_idx) else {
            return Err(crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("page index {page_idx} out of range"),
            )));
        };

        let offset_usize = usize::try_from(entry.offset).map_err(|_| {
            crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "page offset overflows usize",
            ))
        })?;
        // Checked offset arithmetic: a malformed (attacker-influenced)
        // `PageIndexEntry` could carry a huge offset/compressed_size, so
        // every addition must reject overflow rather than wrap.
        let compressed_len = len_u32_to_usize(entry.compressed_size);
        let data_start = offset_usize.checked_add(PAGE_HEADER_SIZE).ok_or_else(|| {
            crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "page data_start overflow",
            ))
        })?;
        let data_end = data_start.checked_add(compressed_len).ok_or_else(|| {
            crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "page data_end overflow",
            ))
        })?;

        // Mapped fast path, only when the page lies inside both the view's extent
        // and the file's live length: the map length is fixed at map time, so a file
        // truncated by another process leaves the view's tail past EOF (SIGBUS on
        // deref). Best-effort: a truncation racing the deref can still fault, which
        // the map contract already forbids. A page failing either guard takes the
        // positional read, which fails closed (UnexpectedEof).
        let mapped: Option<&[u8]> = match self.mmap {
            Some(ref mmap) if data_end <= mmap.len() => {
                let live_ok = match self.file.as_ref().and_then(|f| f.metadata().ok()) {
                    Some(meta) => {
                        // Fail closed on unrepresentable lengths (cannot
                        // happen on 64-bit): treat as "not safely mapped".
                        usize::try_from(meta.len()).is_ok_and(|live| data_end <= live)
                    }
                    // No handle to re-check against (cannot happen while a
                    // map exists): fall back to the positional read.
                    None => false,
                };
                if live_ok {
                    // Checked accessor: never indexes past the recorded
                    // mapping length (`slice` validates the range again).
                    mmap.slice(data_start, compressed_len)
                } else {
                    None
                }
            }
            _ => None,
        };

        let owned;
        let compressed: &[u8] = match mapped {
            Some(slice) => slice,
            None => {
                owned = self.read_page_bytes(data_start as u64, compressed_len)?;
                &owned
            }
        };

        let decompressed = crate::decode_cold_bounded(compressed)?;
        Ok(deserialize_page_lines(&decompressed))
    }

    /// Read `len` bytes at `offset` from the backing file, positionally (no shared
    /// cursor).
    ///
    /// Serves pages beyond the mapped extent (appended since the last load or
    /// compaction) and is the fail-closed fallback when the map cannot safely serve
    /// a page. A short read surfaces as `UnexpectedEof`, never a fault.
    // Skip: a positional-read loop over an OS handle, the io class the strict gate
    // treats as absent-callee; the buffer length carries the bounds.
    #[cfg_attr(trust_verify, trust::skip)]
    fn read_page_bytes(&self, offset: u64, len: usize) -> Result<Vec<u8>, crate::ScrollbackError> {
        let Some(file) = self.file.as_ref() else {
            // In-memory mode: metadata-only stores have no page bytes to read.
            return Err(crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "no memory map available for disk read",
            )));
        };
        // Bound the allocation by the live file length first, so an entry with a huge
        // compressed_size fails closed without committing a multi-GiB buffer.
        let live_len = file.metadata().map_err(crate::ScrollbackError::Io)?.len();
        let end = offset.checked_add(len as u64).ok_or_else(|| {
            crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "page range overflow",
            ))
        })?;
        if end > live_len {
            return Err(crate::ScrollbackError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("page range {offset}..{end} exceeds live file len {live_len}"),
            )));
        }
        let mut buf = vec![0u8; len];
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_exact_at(&mut buf, offset)
                .map_err(crate::ScrollbackError::Io)?;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0usize;
            while done < len {
                let n = file
                    .seek_read(&mut buf[done..], offset.saturating_add(done as u64))
                    .map_err(crate::ScrollbackError::Io)?;
                if n == 0 {
                    return Err(crate::ScrollbackError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "positional read hit EOF inside a page",
                    )));
                }
                // Bounded by `len`; saturating discharges the strict gate's
                // overflow obligation.
                done = done.saturating_add(n);
            }
        }
        Ok(buf)
    }

    /// Drop the whole decompressed-page cache and zero its byte accounting together,
    /// so `cache_bytes` (== Σ `CacheEntry::bytes`) cannot drift. Every cache clear
    /// goes through here.
    pub(crate) fn clear_page_cache(&mut self) {
        self.cache.get_mut().clear();
        self.cache_bytes.set(0);
    }

    /// Byte cost of a decompressed page: its `Vec<Line>` buffer plus each line's
    /// owned content, computed like the cache term of [`Self::calculate_memory_used`].
    pub(crate) fn page_byte_size(lines: &[Line]) -> usize {
        let line_struct_size = std::mem::size_of::<Line>();
        let lines_mem = lines.len().saturating_mul(line_struct_size);
        let contents_mem: usize = lines
            .iter()
            .map(|line| line.memory_used().saturating_sub(line_struct_size))
            .fold(0usize, usize::saturating_add);
        lines_mem.saturating_add(contents_mem)
    }

    /// Add a page to the LRU cache, evicting as needed.
    ///
    /// Bounded by both page count (`cache_size`) and bytes (`cache_byte_limit`); a
    /// page larger than the whole budget is not cached. Without the byte bound a
    /// malicious `.dtrm` with a few pages near the 64 MiB decompression cap would pin
    /// `cache_size × 64 MiB`.
    pub(super) fn cache_page(&self, page_idx: usize, lines: Vec<Line>) {
        if self.cache_size == 0 || self.cache_byte_limit == 0 {
            return;
        }
        let page_bytes = Self::page_byte_size(&lines);
        if page_bytes > self.cache_byte_limit {
            return;
        }
        {
            let mut cache = self.cache.borrow_mut();
            // Evict LRU pages until both limits admit the incoming page. `else break`
            // covers an empty cache (then 0 + page_bytes <= limit, so no loop).
            while cache.len() >= self.cache_size
                || self.cache_bytes.get().saturating_add(page_bytes) > self.cache_byte_limit
            {
                let lru_key = cache
                    .iter()
                    .min_by_key(|(_, e)| e.last_access)
                    .map(|(k, _)| *k);
                if let Some(key) = lru_key {
                    if let Some(evicted) = cache.remove(&key) {
                        self.cache_bytes
                            .set(self.cache_bytes.get().saturating_sub(evicted.bytes));
                    }
                } else {
                    break;
                }
            }
            let counter = self.access_counter.get() + 1;
            self.access_counter.set(counter);
            cache.insert(
                page_idx,
                CacheEntry {
                    lines,
                    last_access: counter,
                    bytes: page_bytes,
                },
            );
            self.cache_bytes
                .set(self.cache_bytes.get().saturating_add(page_bytes));
        }
        self.reset_bytes_used();
    }

    // Back-removal (newest-end) operations.

    /// Count whole pages consumable from the back and remaining boundary trim.
    ///
    /// Returns `(whole_pages, boundary_trim)` where `boundary_trim` is the
    /// number of lines to trim from the boundary page's back.
    fn count_back_pages(&self, n: usize) -> (usize, usize) {
        let mut whole_pages = 0;
        let mut remaining = n;
        let live = self.live_index();
        for entry in live.iter().rev() {
            let page_lines = len_u32_to_usize(entry.line_count);
            let actual_idx = live.len() - 1 - whole_pages;
            let available = if actual_idx == 0 {
                page_lines.saturating_sub(self.front_offset)
            } else {
                page_lines
            };
            if remaining >= available {
                remaining -= available;
                whole_pages += 1;
            } else {
                break;
            }
        }
        (whole_pages, remaining)
    }

    /// Remove the newest `n` lines from the back of the cold tier.
    ///
    /// Drops whole pages from the back without decompression. For the
    /// boundary page (partially within the remove range), decompresses it,
    /// trims the consumed lines from the back, re-compresses, and rewrites
    /// the page at its original file offset.
    ///
    /// Transactional: all fallible work (boundary decompress and re-compress, every
    /// file write/sync/set_len) runs before any in-memory mutation, so on `Err` the
    /// in-memory tier is unchanged and callers can keep operating on it.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count`.
    pub fn truncate_back_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        debug_assert!(
            n <= self.line_count,
            "truncate_back_lines({n}) exceeds line_count({})",
            self.line_count
        );

        let (whole_pages, boundary_trim) = self.count_back_pages(n);

        // --- Phase 1: all fallible CPU work, no in-memory mutation. ---
        // Decompress + re-compress the boundary page (if any) up front.
        let boundary_data = if boundary_trim > 0 {
            let boundary_idx = self.live_index().len() - 1 - whole_pages;
            let lines = self.decompress_page(boundary_idx)?;
            debug_assert!(
                lines.len() >= boundary_trim,
                "decompress returned {} lines but boundary_trim is {}",
                lines.len(),
                boundary_trim,
            );
            let keep = lines.len().saturating_sub(boundary_trim);
            if keep == 0 {
                None
            } else {
                let serialized = crate::line::serialize_lines(&lines[..keep]);
                let compressed = crate::encode_cold_block(&serialized)?;
                Some((compressed, keep))
            }
        } else {
            None
        };

        // Plan the surviving layout from the live view without mutating `self`; the
        // commit phase resets the front-drop state to match.
        let surviving = self.live_index().len() - whole_pages;
        let rewrite_boundary = boundary_trim > 0 && surviving > 0;

        let mut new_index: Vec<PageIndexEntry> = self.live_index()[..surviving].to_vec();
        if rewrite_boundary {
            if let Some((ref compressed, line_count)) = boundary_data {
                // Boundary page keeps its offset but shrinks in place.
                let entry = new_index
                    .last_mut()
                    .expect("surviving > 0 implies a boundary page");
                entry.compressed_size = len_to_u32(compressed.len());
                entry.line_count = len_to_u32(line_count);
            } else {
                // Boundary page fully consumed — drop it too.
                new_index.pop();
            }
        }

        let new_write_offset = match new_index.last() {
            Some(last) => {
                last.offset
                    + PAGE_HEADER_SIZE as u64
                    + len_u32_to_usize(last.compressed_size) as u64
            }
            None => HEADER_SIZE as u64,
        };

        // --- Phase 2: all file I/O, still before any in-memory commit. ---
        // The boundary page (the last surviving page) is rewritten at its offset.
        let boundary_write = if rewrite_boundary {
            boundary_data.as_ref().map(|(compressed, line_count)| {
                let offset = new_index
                    .last()
                    .expect("surviving > 0 implies a boundary page")
                    .offset;
                (offset, compressed.as_slice(), *line_count)
            })
        } else {
            None
        };
        self.write_back_truncation(boundary_write, &new_index, new_write_offset)?;

        // --- Phase 3: commit in-memory state (infallible). ---
        // The new index holds only live pages and the cumulative rebuild starts at
        // zero, so the front-drop cursor and absolute base reset too.
        self.index = new_index;
        self.front_dropped = 0;
        self.cumulative_base = 0;
        self.cumulative_lines.clear();
        self.cumulative_lines.reserve(self.index.len());
        let mut cumulative = 0usize;
        for entry in &self.index {
            cumulative += len_u32_to_usize(entry.line_count);
            self.cumulative_lines.push(cumulative);
        }
        self.write_offset = new_write_offset;

        if n > self.line_count {
            aterm_log::warn!(
                "disk truncate_back_lines({n}) exceeds line_count({}), saturating",
                self.line_count
            );
        }
        self.line_count = self.line_count.saturating_sub(n);

        // Reset front_offset once all pages are gone, or it would skip lines of the
        // next page appended.
        if self.index.is_empty() {
            self.front_offset = 0;
        }

        self.clear_page_cache();
        self.reset_bytes_used();
        Ok(())
    }

    /// File I/O for a back-truncation against the planned layout. It runs before any
    /// in-memory commit, so a failure leaves the in-memory tier unchanged.
    /// `boundary_write` is the `(offset, compressed_bytes, line_count)` of the trimmed
    /// boundary page to rewrite in place; `new_write_offset` is the new logical end of
    /// file.
    fn write_back_truncation(
        &mut self,
        boundary_write: Option<(u64, &[u8], usize)>,
        new_index: &[PageIndexEntry],
        new_write_offset: u64,
    ) -> Result<(), crate::ScrollbackError> {
        if self.file.is_none() {
            return Ok(());
        }
        // Drop the mmap before writing through the File handle.
        if self.mmap.is_some() {
            self.flush_and_drop_mmap()
                .map_err(crate::ScrollbackError::Io)?;
        }
        let io_result = self.write_back_truncation_io(boundary_write, new_index, new_write_offset);
        if io_result.is_err() {
            // A step failed partway. In-memory state is untouched (the caller skips
            // its commit on Err), but the mmap was dropped above: re-map the possibly
            // torn file so pages the write did not touch stay readable. The torn
            // boundary page fails closed in `decompress_page`, and the next load
            // repairs the layout (`scan_pages`).
            self.remap_current_file();
        }
        io_result
    }

    /// All file I/O for a back-truncation: rewrite the trimmed boundary page in
    /// place, update the header, truncate, and re-map. Split from
    /// [`Self::write_back_truncation`] so that can re-map if a step fails partway.
    fn write_back_truncation_io(
        &mut self,
        boundary_write: Option<(u64, &[u8], usize)>,
        new_index: &[PageIndexEntry],
        new_write_offset: u64,
    ) -> Result<(), crate::ScrollbackError> {
        if let Some(ref mut file) = self.file {
            // Rewrite the trimmed boundary page in place.
            if let Some((offset, compressed, line_count)) = boundary_write {
                file.seek(SeekFrom::Start(offset))
                    .map_err(crate::ScrollbackError::Io)?;
                let mut page_header = [0u8; PAGE_HEADER_SIZE];
                page_header[0..4].copy_from_slice(&len_to_u32(compressed.len()).to_le_bytes());
                page_header[4..8].copy_from_slice(&len_to_u32(line_count).to_le_bytes());
                file.write_all(&page_header)
                    .map_err(crate::ScrollbackError::Io)?;
                file.write_all(compressed)
                    .map_err(crate::ScrollbackError::Io)?;
                file.sync_data().map_err(crate::ScrollbackError::Io)?;
            }
            // Update header: page count + physical line count.
            file.seek(SeekFrom::Start(8))
                .map_err(crate::ScrollbackError::Io)?;
            file.write_all(&(new_index.len() as u64).to_le_bytes())
                .map_err(crate::ScrollbackError::Io)?;
            let physical_lines: usize = new_index
                .iter()
                .map(|e| len_u32_to_usize(e.line_count))
                .sum();
            file.write_all(&(physical_lines as u64).to_le_bytes())
                .map_err(crate::ScrollbackError::Io)?;
            file.sync_data().map_err(crate::ScrollbackError::Io)?;
            file.set_len(new_write_offset)
                .map_err(crate::ScrollbackError::Io)?;
            file.sync_data().map_err(crate::ScrollbackError::Io)?;
            // Re-map after the resize.
            // SAFETY: the File is exclusively owned by this tier.
            if new_write_offset > HEADER_SIZE as u64 {
                self.mmap =
                    Some(unsafe { MmapMut::map_mut(&*file).map_err(crate::ScrollbackError::Io)? });
            } else {
                self.mmap = None;
            }
        }
        // This path wrote the header counters and issued its own barriers, so
        // any deferred-append debt is settled (see disk_write.rs).
        self.mark_appends_synced();
        Ok(())
    }

    /// Best-effort re-map from the file's current length after an error path dropped
    /// the mmap (partial write-back, failed compaction). Leaves `mmap = None` if
    /// mapping fails; reads then return a clean error rather than fault.
    pub(super) fn remap_current_file(&mut self) {
        let Some(file) = self.file.as_ref() else {
            self.mmap = None;
            return;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len > HEADER_SIZE as u64 {
            // SAFETY: the File is exclusively owned by this tier. Its contents may be
            // torn; `decompress_page` re-checks the live length and page bounds on
            // every read, so a torn region fails closed instead of faulting.
            self.mmap = unsafe { MmapMut::map_mut(file).ok() };
        } else {
            self.mmap = None;
        }
    }
}
