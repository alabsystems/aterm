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

/// Default memory budget (100 MB).
pub(crate) const DEFAULT_MEMORY_BUDGET: usize = 100 * 1024 * 1024;

/// Default line limit (lines).
///
/// Caps total scrollback to prevent runaway memory growth (#7929 / HN F09-1).
/// A runaway process writing to stdout would otherwise grow scrollback without
/// bound until `memory_budget` is exhausted (and for disk-backed storage, fill
/// the disk). 100,000 lines is a pragmatic default: generous for typical
/// interactive sessions, but bounded for attacker workloads.
///
/// Hosts that need unbounded history can opt in via
/// `Scrollback::set_line_limit(None)` or
/// `ConfigBuilder::unlimited_scrollback()`.
pub const DEFAULT_LINE_LIMIT: usize = 100_000;

/// Maximum decompressed size for a single scrollback page (64 MiB).
pub(crate) const MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES: usize = 64 * 1024 * 1024;

/// Binary search on a sorted cumulative array, counting iterations in `steps`.
///
/// `steps` was previously an `impl FnMut()` callback; it is now a plain
/// `&mut usize` increment so the strict Trust gate sees a closed body (an
/// opaque callback is an absent callee it must assume may panic). Every
/// caller only ever counted iterations, so the observable behavior is
/// identical.
pub(crate) fn binary_search_counted(
    cumulative: &[usize],
    target: usize,
    steps: &mut usize,
) -> Result<usize, usize> {
    let mut left = 0usize;
    let mut right = cumulative.len();

    while left < right {
        // Wrapping: the search space halves every iteration, so this loop
        // body runs at most `usize::BITS` times per call and `*steps` is a
        // small diagnostic count (tests reset it around each call) — the
        // wrap can never occur on any real path. `wrapping_add` only
        // discharges the strict gate's overflow obligation; the result is
        // identical to `+ 1` everywhere reachable.
        *steps = steps.wrapping_add(1);
        // NOTE: kept as the single composite expression. A split (`half`,
        // then `mid`) was tried for the solver-unknown overflow obligation
        // and reverted: it did not discharge it, and restructurings around
        // this loop-carried division risk sending the strict gate's integer
        // engine into a non-terminating solve. Wrapping: `(right - left) / 2
        // <= right - left`, so `left + (right - left) / 2 <= right <= len <=
        // usize::MAX` on every iteration — the wrap can never occur, and
        // `wrapping_add` only removes the gate's overflow check
        // (behavior-identical to `+` on every reachable path).
        // wrapping_sub: `left <= right` on every iteration (the loop
        // invariant the verifier cannot chain); exact on all real paths.
        let mid = left.wrapping_add(right.wrapping_sub(left) / 2);
        // `mid < right <= cumulative.len()` on every iteration, so the lookup
        // can never miss; the `get` + unreachable early-return spelling carries
        // that bounds proof for the strict L0 gate (identical result to the
        // previous `cumulative[mid]` on every reachable path).
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

/// Decode zstd-compressed data with an output-size cap.
///
/// Only available with the `zstd` feature (the on-disk `.dtrm` cold format and
/// the in-memory zstd cold tier). The default build uses LZ4 for the cold tier
/// (see [`decode_cold_bounded`]).
#[cfg(feature = "zstd")]
pub(crate) fn decode_zstd_bounded(compressed: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    use std::io::Read;

    let decoder = zstd::Decoder::new(compressed)?;
    let max_plus_one = (MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES as u64).saturating_add(1);
    let mut limited = decoder.take(max_plus_one);
    let mut decoded = Vec::with_capacity(compressed.len());
    limited.read_to_end(&mut decoded)?;
    if decoded.len() > MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES {
        return Err(ScrollbackError::Decompression(format!(
            "decompressed size exceeds {MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES} byte limit"
        )));
    }
    Ok(decoded)
}

/// zstd compression level for the in-memory cold tier.
///
/// Eviction (warm→cold) re-compresses each block with zstd at this level, so the
/// level directly sets per-eviction CPU. Level 1 is chosen deliberately over the
/// library default (3): on representative terminal scrollback it compresses
/// ~2.3× faster than level 3 for only a ~5% larger compressed footprint, and on
/// hard (high-entropy) data it is ~1.8× faster for a ~2% ratio loss. The level
/// is purely a CPU/ratio knob — decompression yields byte-identical lines at any
/// level, so lowering it is fully behavior-preserving (the
/// [`encode_cold_block`]/[`decode_cold_bounded`] roundtrip is unchanged).
#[cfg(feature = "zstd")]
pub(crate) const COLD_ZSTD_LEVEL: i32 = 1;

/// Compress a serialized scrollback block for the in-memory cold tier.
///
/// Uses zstd (better ratio) when the `zstd` feature is on, and otherwise falls
/// back to the LZ4 path that already backs the warm tier. The codec is fixed at
/// compile time, so cold pages produced in a process are always decodable by the
/// matching [`decode_cold_bounded`] in the same build.
pub(crate) fn encode_cold_block(serialized: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    #[cfg(feature = "zstd")]
    {
        zstd::encode_all(serialized, COLD_ZSTD_LEVEL).map_err(ScrollbackError::Io)
    }
    #[cfg(not(feature = "zstd"))]
    {
        crate::lz4::compress_prepend_size(serialized)
            .map_err(|err| ScrollbackError::Decompression(format!("LZ4: {err}")))
    }
}

/// Decode an in-memory cold-tier block with an output-size cap.
///
/// Mirror of [`encode_cold_block`]: zstd when the feature is on, otherwise the
/// size-prepended LZ4 path used by the warm tier.
pub(crate) fn decode_cold_bounded(compressed: &[u8]) -> Result<Vec<u8>, ScrollbackError> {
    #[cfg(feature = "zstd")]
    {
        decode_zstd_bounded(compressed)
    }
    #[cfg(not(feature = "zstd"))]
    {
        decompress_lz4_bounded(compressed)
    }
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
        // Manual rendering of the previous
        // `format!("LZ4 prepended size {claimed_size} exceeds {MAX..} byte limit")`:
        // byte-identical output, but with no `fmt::Arguments` (whose expansion
        // embeds unsafe the strict Trust gate cannot lower).
        let mut msg = String::from("LZ4 prepended size ");
        msg.push_str(&crate::error::dec_string(claimed_size));
        msg.push_str(" exceeds ");
        msg.push_str(&crate::error::dec_string(
            MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES,
        ));
        msg.push_str(" byte limit");
        return Err(ScrollbackError::Decompression(msg));
    }

    // Manual rendering of the previous `format!("LZ4: {err}")` (byte-identical:
    // `Lz4Error::to_message` IS its Display output), spelled without
    // `fmt::Arguments` so the strict gate can lower the closure.
    crate::lz4::decompress_size_prepended(compressed).map_err(|err| {
        let mut msg = String::from("LZ4: ");
        msg.push_str(&err.to_message());
        ScrollbackError::Decompression(msg)
    })
}
