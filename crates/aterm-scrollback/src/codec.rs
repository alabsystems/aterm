// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared compression helpers and search utilities for scrollback tiers.

use super::ScrollbackError;

/// Default lines per compressed block.
pub(crate) const DEFAULT_BLOCK_SIZE: usize = 100;

/// Default hot tier limit (lines).
pub(crate) const DEFAULT_HOT_LIMIT: usize = 1000;

/// Default warm tier limit (lines).
pub(crate) const DEFAULT_WARM_LIMIT: usize = 10000;

/// Default memory budget (100 MB) for [`Scrollback::with_defaults`](crate::Scrollback::with_defaults).
pub const DEFAULT_MEMORY_BUDGET: usize = 100 * 1024 * 1024;

/// Default maximum scrollback line count.
pub const DEFAULT_LINE_LIMIT: usize = 100_000;

/// Maximum decompressed size for a single scrollback page (64 MiB).
pub(crate) const MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES: usize = 64 * 1024 * 1024;

/// Binary search on a sorted cumulative array, counting iterations in `steps`.
pub(crate) fn binary_search_counted(
    cumulative: &[usize],
    target: usize,
    steps: &mut usize,
) -> Result<usize, usize> {
    let mut left = 0usize;
    let mut right = cumulative.len();

    while left < right {
        *steps = steps.wrapping_add(1);
        let mid = left.wrapping_add(right.wrapping_sub(left) / 2);
        let Some(&entry) = cumulative.get(mid) else {
            return Err(left);
        };
        match entry.cmp(&target) {
            std::cmp::Ordering::Less => left = mid.saturating_add(1),
            std::cmp::Ordering::Greater => right = mid,
            std::cmp::Ordering::Equal => return Ok(mid),
        }
    }

    Err(left)
}

/// Compress a serialized scrollback block for the cold tier.
pub(crate) fn encode_cold_block(serialized: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    crate::lz4::compress_prepend_size(serialized)
        .map_err(|err| ScrollbackError::Decompression(format!("LZ4: {err}")))
}

/// Decode a cold-tier block capped at [`MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES`].
pub(crate) fn decode_cold_bounded(compressed: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    decompress_lz4_bounded(compressed)
}

/// Decompress LZ4 data with a validated prepended size.
pub(crate) fn decompress_lz4_bounded(compressed: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    if compressed.len() < 4 {
        return Err(ScrollbackError::Decompression(
            "LZ4 data too short for size prefix".to_string(),
        ));
    }

    let claimed_size =
        u32::from_le_bytes([compressed[0], compressed[1], compressed[2], compressed[3]]) as usize;
    if claimed_size > MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
        let mut msg = String::from("LZ4 prepended size ");
        msg.push_str(&crate::error::dec_string(claimed_size));
        msg.push_str(" exceeds ");
        msg.push_str(&crate::error::dec_string(
            MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES,
        ));
        msg.push_str(" byte limit");
        return Err(ScrollbackError::Decompression(msg));
    }

    crate::lz4::decompress_size_prepended(compressed).map_err(|err| {
        let mut msg = String::from("LZ4: ");
        msg.push_str(&err.to_message());
        ScrollbackError::Decompression(msg)
    })
}
