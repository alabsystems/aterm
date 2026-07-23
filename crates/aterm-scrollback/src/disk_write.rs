// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Write path for [`DiskColdTier`] — compressed page append.

use super::DiskColdTier;
use crate::disk_format::{PAGE_HEADER_SIZE, PageIndexEntry, len_to_u32};
use crate::mmap::MmapMut;
use std::io::{self, Seek, SeekFrom, Write};

impl DiskColdTier {
    /// Push compressed data from a warm block.
    ///
    /// The data is already Zstd compressed.
    ///
    /// Uses a transactional pattern: internal state (line_count, index,
    /// cumulative_lines, write_offset) is only updated after all I/O
    /// operations succeed. On I/O failure the state remains consistent
    /// with the previously committed page (#7575).
    pub(crate) fn push_compressed(
        &mut self,
        compressed: &[u8],
        line_count: usize,
    ) -> io::Result<()> {
        if compressed.is_empty() || line_count == 0 {
            return Ok(());
        }

        // Drop the current view before appending. The cold-tier mapping is
        // only ever read (all writes below go through the File), so it holds no
        // dirty pages: flushing it here would issue a redundant full
        // FlushViewOfFile + FlushFileBuffers. Just unmap — durability of the
        // appended page is provided by the File::sync_data barriers below.
        if self.file.is_some() {
            self.mmap = None;
        }

        if let Some(ref mut file) = self.file {
            // Prepare values for the transactional update.
            let new_write_offset = self
                .write_offset
                .saturating_add(PAGE_HEADER_SIZE as u64)
                .saturating_add(compressed.len() as u64);
            let new_line_count = self.line_count.saturating_add(line_count);
            let new_page_count = self.index.len() + 1;
            let new_cumulative = self
                .cumulative_lines
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(line_count);
            let entry = PageIndexEntry {
                offset: self.write_offset,
                compressed_size: len_to_u32(compressed.len()),
                line_count: len_to_u32(line_count),
            };

            // --- I/O block: all disk writes happen here. If any step fails,
            // we return early WITHOUT modifying in-memory state. ---

            // Write page header
            let mut page_header = [0u8; PAGE_HEADER_SIZE];
            page_header[0..4].copy_from_slice(&len_to_u32(compressed.len()).to_le_bytes());
            page_header[4..8].copy_from_slice(&len_to_u32(line_count).to_le_bytes());

            file.seek(SeekFrom::Start(self.write_offset))?;
            file.write_all(&page_header)?;
            file.write_all(compressed)?;
            // Barrier: page data must be durable before header update (#5917).
            file.sync_data()?;

            // Update header with new counts
            file.seek(SeekFrom::Start(8))?;
            file.write_all(&(new_page_count as u64).to_le_bytes())?;
            file.write_all(&(new_line_count as u64).to_le_bytes())?;
            file.sync_data()?;

            // Refresh mmap after file growth from append above. The cold tier
            // only reads through this view, so map it read-only: no dirty
            // pages, and dropping it (on the next append/compaction) is
            // flush-free.
            // SAFETY: File is exclusively owned by this DiskStore; we just extended it
            // via write_all and set_len, so the new size is consistent with the mapping.
            let new_mmap = unsafe { MmapMut::map_read(&*file) }?;

            // --- All I/O succeeded. Commit in-memory state atomically. ---
            self.line_count = new_line_count;
            self.write_offset = new_write_offset;
            self.index.push(entry);
            self.cumulative_lines.push(new_cumulative);
            self.mmap = Some(new_mmap);
        } else {
            // In-memory only mode - just update counts
            let entry = PageIndexEntry {
                offset: 0,
                compressed_size: len_to_u32(compressed.len()),
                line_count: len_to_u32(line_count),
            };
            self.index.push(entry);
            self.line_count = self.line_count.saturating_add(line_count);
            let cumulative = self
                .cumulative_lines
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(line_count);
            self.cumulative_lines.push(cumulative);
        }

        self.reset_bytes_used();

        Ok(())
    }
}
