// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for `DiskColdTier` — extracted from `disk.rs` (#2100).
//! Split into submodules for #5931.

use super::super::line::serialize_lines;
use super::*;
use crate::ScrollbackError;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use aterm_tempfile::tempdir;

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
}

pub(super) fn create_test_page(line_count: usize, prefix: &str) -> (Vec<u8>, usize) {
    let lines: Vec<Line> = (0..line_count)
        .map(|i| Line::from(&*format!("{prefix}-Line{i}")))
        .collect();
    let serialized = serialize_lines(&lines);
    let compressed = zstd::encode_all(serialized.as_slice(), 3).unwrap();
    (compressed, line_count)
}

pub(super) fn read_header_counts(path: &Path) -> (u64, u64) {
    let mut file = File::open(path).unwrap();
    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header).unwrap();
    let page_count = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let line_count = u64::from_le_bytes(header[16..24].try_into().unwrap());
    (page_count, line_count)
}

mod basic;
mod compaction;
mod crash_recovery;
mod truncation;
