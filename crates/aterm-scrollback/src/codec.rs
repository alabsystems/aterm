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

/// Default memory budget (100 MB): what [`Scrollback::with_defaults`] lets a
/// store hold, and the budget a grid with no store sizes its history
/// allowances from.
///
/// [`Scrollback::with_defaults`]: crate::Scrollback::with_defaults
pub const DEFAULT_MEMORY_BUDGET: usize = 100 * 1024 * 1024;

/// Default line limit (lines): bounds a runaway writer short of exhausting the
/// memory budget. Unbounded history is opt-in via
/// `Scrollback::set_line_limit(None)` or a `TerminalConfig` whose
/// `scrollback_limit` is `None`.
pub const DEFAULT_LINE_LIMIT: usize = 100_000;

/// Maximum decompressed size for a single scrollback page (64 MiB).
///
/// One limit for writer and reader: every page decoder admits this much (the
/// LZ4 cap in `lz4.rs` is this constant) and `WarmTier::push_block` pages by
/// it, so the store never writes a block it cannot read back. Cold pages are
/// one per warm block, so the bound carries to them.
pub(crate) const MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES: usize = 64 * 1024 * 1024;

/// Binary search on a sorted cumulative array, counting iterations in `steps`.
///
/// `steps` is a plain `&mut usize`, not a callback, so the Trust lane (`ship trust run`)
/// sees a closed body (an opaque callback is an absent callee it must assume
/// may panic).
pub(crate) fn binary_search_counted(
    cumulative: &[usize],
    target: usize,
    steps: &mut usize,
) -> Result<usize, usize> {
    let mut left = 0usize;
    let mut right = cumulative.len();

    while left < right {
        // Wrapping: the loop runs at most `usize::BITS` times, so this small
        // diagnostic count never wraps; it only discharges the strict gate's
        // overflow obligation.
        *steps = steps.wrapping_add(1);
        // Keep this one composite expression: splitting it did not discharge
        // the overflow obligation, and restructuring this loop-carried division
        // can send the strict gate's integer engine into a non-terminating
        // solve. Neither op wraps: `left <= right` and
        // `left + (right - left) / 2 <= right <= len`.
        let mid = left.wrapping_add(right.wrapping_sub(left) / 2);
        // `mid < right <= len`, so the lookup never misses; the `get` spelling
        // carries that bounds proof for the strict L0 gate.
        let Some(&entry) = cumulative.get(mid) else {
            return Err(left);
        };
        match entry.cmp(&target) {
            // saturating_add: `mid < right <= len` — exact on all real paths.
            std::cmp::Ordering::Less => left = mid.saturating_add(1),
            std::cmp::Ordering::Greater => right = mid,
            std::cmp::Ordering::Equal => return Ok(mid),
        }
    }

    Err(left)
}

/// Compress a serialized scrollback block for the cold tier.
///
/// The cold codec is the size-prepended LZ4 path that already backs the warm
/// tier, so a cold page decodes with [`decode_cold_bounded`] in any build.
pub(crate) fn encode_cold_block(serialized: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    crate::lz4::compress_prepend_size(serialized)
        .map_err(|err| ScrollbackError::Decompression(format!("LZ4: {err}")))
}

/// Decode a cold-tier block with an output-size cap.
///
/// Mirror of [`encode_cold_block`]: the size-prepended LZ4 path used by the
/// warm tier, capped at [`MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES`].
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
        // Built by hand, not with `format!`: `fmt::Arguments` embeds unsafe the
        // Trust lane (`ship trust run`) cannot lower.
        let mut msg = String::from("LZ4 prepended size ");
        msg.push_str(&crate::error::dec_string(claimed_size));
        msg.push_str(" exceeds ");
        msg.push_str(&crate::error::dec_string(
            MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES,
        ));
        msg.push_str(" byte limit");
        return Err(ScrollbackError::Decompression(msg));
    }

    // Built by hand for the same reason (`to_message` is `Lz4Error`'s Display).
    crate::lz4::decompress_size_prepended(compressed).map_err(|err| {
        let mut msg = String::from("LZ4: ");
        msg.push_str(&err.to_message());
        ScrollbackError::Decompression(msg)
    })
}
