// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Disk cold tier file format constants and helpers.
//!
//! Defines the on-disk layout for `.dtrm` cold storage files:
//! 32-byte file header followed by page headers with Zstd-compressed data.

use std::io;

/// Magic bytes identifying a aterm cold storage file.
pub(crate) const MAGIC: &[u8; 4] = b"DTRM";

/// Current file format version.
pub(crate) const VERSION: u32 = 1;

/// File header size in bytes.
pub(crate) const HEADER_SIZE: usize = 32;

/// Page header size in bytes.
pub(crate) const PAGE_HEADER_SIZE: usize = 8;

/// Default LRU cache size (number of decompressed pages).
pub(crate) const DEFAULT_CACHE_SIZE: usize = 8;

/// Default LRU cache byte budget for decompressed cold pages.
pub(crate) const DEFAULT_CACHE_BYTE_LIMIT: usize = 8 * 1024 * 1024;

// ============================================================================
// Safe Cast Helpers for Disk Serialization
// ============================================================================

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
    /// Number of lines in this page.
    ///
    /// Persisted in page headers for:
    /// - File format compatibility (read during load)
    /// - Kani proof verification of line count consistency
    ///
    /// After construction, cumulative_lines is used for lookups instead.
    /// Debug builds assert this matches cumulative line counts.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(crate) line_count: u32,
}
