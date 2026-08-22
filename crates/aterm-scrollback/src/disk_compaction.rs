// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Compaction, clear, sync, and mmap lifecycle for [`DiskColdTier`].

use super::DiskColdTier;
use crate::disk_format::{
    HEADER_SIZE, MAGIC, PAGE_HEADER_SIZE, PageIndexEntry, VERSION, len_to_u32, len_u32_to_usize,
};
use crate::line::{deserialize_page_lines, serialize_lines};
use crate::mmap::MmapMut;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

impl DiskColdTier {
    /// Rewrite surviving pages contiguously to reclaim dead space at the
    /// front of the file. Uses atomic temp-file + rename for crash safety.
    ///
    /// Called from `truncate_front_lines` when `dead_bytes() > live_bytes()`
    /// (file is >50% garbage). Compaction failure is non-fatal — the file
    /// works fine with dead space; the next rotation will retry.
    pub(super) fn compact(&mut self) -> io::Result<()> {
        if self.file.is_none() {
            return Ok(());
        }
        // `compact_inner` drops the mmap up front and only re-establishes it on the
        // success path (reopen_after_compact). On any error before the rename — a
        // temp File::create, page read, zstd decode of a corrupt first page, or
        // disk-full write — self.file / self.index / front_offset are untouched;
        // a failed rename reopens the original path before returning. Either way,
        // best-effort re-map the original file to keep cold reads working. Without
        // this, every subsequent decompress_page returns "no memory map available
        // for disk read" until the next append, contradicting the documented
        // "compaction failure is non-fatal — the file works fine with dead space"
        // contract. Mirrors write_back_truncation's partial-write recovery. If the
        // post-rename reopen failed, self.file is None and this leaves mmap = None
        // (reads fail closed until the store is reloaded).
        let result = self.compact_inner();
        if result.is_err() {
            self.remap_current_file();
        }
        result
    }

    fn compact_inner(&mut self) -> io::Result<()> {
        self.flush_and_drop_mmap()?;

        let tmp_path = self.path.with_extension("dtrm.tmp");
        let mut tmp = File::create(&tmp_path)?;

        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        tmp.write_all(&header)?;

        let mut new_offset = HEADER_SIZE as u64;
        // LIVE entries only: dead-prefix entries describe already-dropped
        // pages and must not be resurrected into the compacted file. Direct
        // field slicing (not the live_index() method) keeps the borrow
        // disjoint from the `self.file` mutable borrow below.
        let live_from = self.front_dropped;
        let live_entries = self.index.get(live_from..).unwrap_or(&[]);
        let mut new_index = Vec::with_capacity(live_entries.len());
        let trim_front = self.front_offset;
        let file = self
            .file
            .as_mut()
            .expect("invariant: file exists after is_some guard");

        for (i, entry) in live_entries.iter().enumerate() {
            let (idx_entry, size) = if i == 0 && trim_front > 0 {
                Self::compact_page_trimmed(file, entry, trim_front, new_offset, &mut tmp)?
            } else {
                Self::compact_page_verbatim(file, entry, new_offset, &mut tmp)?
            };
            new_index.push(idx_entry);
            new_offset += size;
        }

        Self::write_compact_header(&mut tmp, &new_index)?;
        drop(tmp);
        // Close our own destination handle BEFORE renaming over it: Windows can
        // only replace a name with open handles when every holder opened with
        // FILE_SHARE_DELETE and the volume supports POSIX-semantics rename
        // (NTFS). Holding self.file across the rename made compaction fail on
        // every rotation on exFAT/FAT32 scrollback dirs, so front dead bytes
        // were never reclaimed and the file grew without bound.
        self.file = None;
        if let Err(e) = with_av_retry(|| std::fs::rename(&tmp_path, &self.path)) {
            // The original file still owns self.path; reopen it so the tier
            // keeps operating with dead space instead of silently degrading to
            // memory-only appends (the reopen error, vanishingly unlikely for a
            // file we just read, is secondary to the rename error we return).
            self.file = with_av_retry(|| open_rw(&self.path)).ok();
            return Err(e);
        }
        self.reopen_after_compact(new_index, new_offset, trim_front > 0)
    }

    /// Decompress the first page, trim the consumed prefix, recompress (#5942).
    fn compact_page_trimmed(
        file: &mut File,
        entry: &PageIndexEntry,
        trim: usize,
        write_offset: u64,
        tmp: &mut File,
    ) -> io::Result<(PageIndexEntry, u64)> {
        let mut compressed = vec![0u8; len_u32_to_usize(entry.compressed_size)];
        file.seek(SeekFrom::Start(entry.offset + PAGE_HEADER_SIZE as u64))?;
        file.read_exact(&mut compressed)?;

        let decompressed = crate::decode_zstd_bounded(&compressed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let lines = deserialize_page_lines(&decompressed);
        if trim > lines.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "front_offset ({trim}) exceeds deserialized line count ({})",
                    lines.len()
                ),
            ));
        }
        let trimmed = &lines[trim..];
        let recompressed =
            zstd::encode_all(serialize_lines(trimmed).as_slice(), crate::COLD_ZSTD_LEVEL)?;

        let mut page_header = [0u8; PAGE_HEADER_SIZE];
        page_header[0..4].copy_from_slice(&len_to_u32(recompressed.len()).to_le_bytes());
        page_header[4..8].copy_from_slice(&len_to_u32(trimmed.len()).to_le_bytes());
        tmp.write_all(&page_header)?;
        tmp.write_all(&recompressed)?;

        let size = PAGE_HEADER_SIZE as u64 + recompressed.len() as u64;
        let idx = PageIndexEntry {
            offset: write_offset,
            compressed_size: len_to_u32(recompressed.len()),
            line_count: len_to_u32(trimmed.len()),
        };
        Ok((idx, size))
    }

    /// Copy a page verbatim to the compacted file.
    fn compact_page_verbatim(
        file: &mut File,
        entry: &PageIndexEntry,
        write_offset: u64,
        tmp: &mut File,
    ) -> io::Result<(PageIndexEntry, u64)> {
        let page_size = PAGE_HEADER_SIZE as u64 + u64::from(entry.compressed_size);
        let mut buf = vec![0u8; page_size as usize];
        file.seek(SeekFrom::Start(entry.offset))?;
        file.read_exact(&mut buf)?;
        tmp.write_all(&buf)?;

        let idx = PageIndexEntry {
            offset: write_offset,
            compressed_size: entry.compressed_size,
            line_count: entry.line_count,
        };
        Ok((idx, page_size))
    }

    /// Write final page/line counts to the compacted file header.
    fn write_compact_header(tmp: &mut File, index: &[PageIndexEntry]) -> io::Result<()> {
        let physical_lines: usize = index.iter().map(|e| len_u32_to_usize(e.line_count)).sum();
        tmp.seek(SeekFrom::Start(8))?;
        tmp.write_all(&(index.len() as u64).to_le_bytes())?;
        tmp.write_all(&(physical_lines as u64).to_le_bytes())?;
        tmp.sync_data()
    }

    /// Reopen the compacted file and rebuild in-memory state.
    fn reopen_after_compact(
        &mut self,
        new_index: Vec<PageIndexEntry>,
        new_offset: u64,
        had_front_trim: bool,
    ) -> io::Result<()> {
        let opened = with_av_retry(|| open_rw(&self.path));
        // Adopt the compacted layout BEFORE the fallible steps: the rename already
        // succeeded, so self.path IS the compacted file, and stale metadata would
        // desynchronize the tier from what a reload sees. If the reopen above
        // failed, self.file stays None (closed pre-rename) and reads fail closed
        // ("no memory map available") rather than serving old offsets.
        self.index = new_index;
        // Fresh, all-live index: reset the front-drop cursor and absolute
        // base along with the zero-based cumulative rebuild below.
        self.front_dropped = 0;
        self.cumulative_base = 0;
        self.write_offset = new_offset;

        if had_front_trim {
            // Consumed prefix is now physically trimmed — reset front_offset (#5942).
            self.front_offset = 0;
        }

        // Rebuild cumulative_lines to match updated page line counts.
        self.cumulative_lines.clear();
        let mut cumulative = 0;
        for entry in &self.index {
            cumulative += len_u32_to_usize(entry.line_count);
            self.cumulative_lines.push(cumulative);
        }

        self.clear_page_cache();
        self.reset_bytes_used();
        // The compacted file carries freshly written, synced header counters
        // (write_compact_header): settle any deferred-append debt.
        self.mark_appends_synced();

        let file = opened?;
        // Map LAST, and treat a mapping failure as NON-fatal: file/index/path are
        // already consistent, so a failed map just leaves reads returning "no memory
        // map available" until the next append re-maps (recoverable) rather than
        // losing appended data. SAFETY: the File is exclusively owned by this tier.
        self.mmap = if new_offset > HEADER_SIZE as u64 {
            unsafe { MmapMut::map_mut(&file) }.ok()
        } else {
            None
        };
        self.file = Some(file);
        Ok(())
    }

    /// Clear all data.
    pub fn clear(&mut self) -> io::Result<()> {
        self.index.clear();
        self.cumulative_lines.clear();
        self.front_dropped = 0;
        self.cumulative_base = 0;
        self.line_count = 0;
        self.front_offset = 0;
        self.clear_page_cache();
        self.access_counter.set(0);
        self.write_offset = HEADER_SIZE as u64;

        // Truncate file if we have one
        if self.file.is_some() {
            self.flush_and_drop_mmap()?;
        }

        if let Some(ref mut file) = self.file {
            file.set_len(HEADER_SIZE as u64)?;
            file.seek(SeekFrom::Start(8))?;
            file.write_all(&0u64.to_le_bytes())?; // page_count
            file.write_all(&0u64.to_le_bytes())?; // line_count
            file.sync_data()?;
        }
        // Header written + synced above: settle any deferred-append debt
        // (see disk_write.rs).
        self.mark_appends_synced();

        self.reset_bytes_used();

        Ok(())
    }

    /// Sync changes to disk.
    #[cfg(test)]
    pub fn sync(&mut self) -> io::Result<()> {
        // Catch the deferred append header up first so an explicit sync()
        // leaves the on-disk header consistent with the in-memory index.
        self.sync_appends()?;
        if let Some(ref mmap) = self.mmap {
            mmap.flush()?;
        }
        if let Some(ref mut file) = self.file {
            file.sync_all()?;
        }
        Ok(())
    }

    /// Ensure mapped data is flushed before unmapping.
    ///
    /// We explicitly drop the mmap to avoid relying on field drop order.
    pub(super) fn flush_and_drop_mmap(&mut self) -> io::Result<()> {
        if let Some(ref mmap) = self.mmap {
            mmap.flush()?;
        }
        self.mmap = None;
        Ok(())
    }
}

fn open_rw(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// Run `op`, retrying briefly on Windows when it fails with a sharing/access
/// error: antivirus scanners and indexers transiently hold handles without
/// FILE_SHARE_DELETE on files in the scrollback dir right after they change,
/// which blocks rename-over and incompatible re-opens.
fn with_av_retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(windows)]
    {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let mut delay_ms = 5u64;
        for _ in 0..4 {
            match op() {
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                    ) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms *= 2;
                }
                other => return other,
            }
        }
    }
    op()
}
