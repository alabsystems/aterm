// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! On-disk layout of `.dtrm` cold storage files: a 32-byte file header, then
//! pages, each an 8-byte page header and a cold-codec (size-prepended LZ4) block.

use std::io;

/// Magic bytes identifying an aterm cold storage file.
pub(crate) const MAGIC: &[u8; 4] = b"DTRM";

/// Current file format version.
///
/// 2: pages are the cold codec's size-prepended LZ4 (`encode_cold_block`).
/// Version 1 pages were zstd frames; a v1 file is refused at load
/// (`unsupported version`) rather than misread, since the two page encodings
/// share every header field.
pub(crate) const VERSION: u32 = 2;

/// File header size in bytes.
pub(crate) const HEADER_SIZE: usize = 32;

/// Page header size in bytes.
pub(crate) const PAGE_HEADER_SIZE: usize = 8;

/// Default LRU cache size (number of decompressed pages).
pub(crate) const DEFAULT_CACHE_SIZE: usize = 8;

/// Default LRU cache byte budget for decompressed cold pages.
pub(crate) const DEFAULT_CACHE_BYTE_LIMIT: usize = 8 * 1024 * 1024;

/// Convert a u64 to usize, erroring if it exceeds platform capacity.
#[inline]
pub(crate) fn len_u64_to_usize(len: u64) -> io::Result<usize> {
    usize::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "length exceeds platform address space",
        )
    })
}

/// Convert a u32 to usize (always succeeds on 32-bit+ platforms).
#[inline]
pub(crate) fn len_u32_to_usize(len: u32) -> usize {
    usize::try_from(len).unwrap_or(usize::MAX)
}

/// Convert a usize to u32, saturating at u32::MAX.
#[inline]
pub(crate) fn len_to_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Configuration for disk-backed cold tier.
#[derive(Debug, Clone)]
pub struct DiskColdConfig {
    /// Path to the storage file.
    pub path: std::path::PathBuf,
    /// Number of decompressed pages to cache.
    pub cache_size: usize,
    /// Maximum bytes for decompressed cached pages.
    pub cache_byte_limit: usize,
}

impl DiskColdConfig {
    /// Create a new config with the given path.
    #[must_use]
    pub(crate) fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache_size: DEFAULT_CACHE_SIZE,
            cache_byte_limit: DEFAULT_CACHE_BYTE_LIMIT,
        }
    }

    /// Set the cache size.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_cache_size(mut self, size: usize) -> Self {
        self.cache_size = size.max(1);
        self
    }

    /// Set the decompressed-page cache byte budget.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_cache_byte_limit(mut self, bytes: usize) -> Self {
        self.cache_byte_limit = bytes.max(1);
        self
    }
}

/// Page index entry (in-memory only).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PageIndexEntry {
    /// Byte offset of page header in file.
    pub(crate) offset: u64,
    /// Compressed size in bytes (excluding page header).
    pub(crate) compressed_size: u32,
    /// Number of lines in this page, as persisted in its page header.
    ///
    /// Lookups use `cumulative_lines`; debug builds check the two agree on load.
    pub(crate) line_count: u32,
}
