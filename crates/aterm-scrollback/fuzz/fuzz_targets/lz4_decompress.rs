// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fuzzes the scrollback LZ4 wrapper: decompression never panics, round-trips
//! are exact, oversized size prefixes are rejected, truncations are handled.
//!
//! STOCK EXCEPTION: Trust ships no cargo-fuzz and no ASan runtime, so this lane
//! runs on a stock nightly:
//!
//! ```bash
//! cd crates/aterm-scrollback
//! cargo +nightly fuzz run lz4_decompress -- -max_total_time=60 -timeout=10
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;

use aterm_scrollback::lz4::{compress_prepend_size, decompress_size_prepended};

fuzz_target!(|data: &[u8]| {
    // Arbitrary input: Ok or Err, never a panic (on-disk pages can be corrupt
    // or tampered with).
    let _ = decompress_size_prepended(data);

    // Round-trip, capped at 256 KiB (well above any real page) to keep the
    // fuzzer off huge allocations.
    if data.len() <= 256 * 1024 {
        if let Ok(compressed) = compress_prepend_size(data) {
            let decompressed = decompress_size_prepended(&compressed)
                .expect("round-trip: decompression of our own compressed data must succeed");
            assert_eq!(
                decompressed,
                data,
                "round-trip: decompressed output differs from original input ({} bytes)",
                data.len()
            );
        }
    }

    // A size prefix above the 64 MiB page cap is rejected without allocating.
    if data.len() >= 4 {
        let bomb_size: u32 = 65 * 1024 * 1024;
        let mut bomb_input = bomb_size.to_le_bytes().to_vec();
        bomb_input.extend_from_slice(&data[4..]);
        let result = decompress_size_prepended(&bomb_input);
        assert!(
            result.is_err(),
            "allocation bomb: 65 MiB size prefix must be rejected"
        );
    }

    // Every truncation of valid output is handled without a panic (small
    // inputs only, to keep the fuzzer fast).
    if data.len() <= 512 {
        if let Ok(compressed) = compress_prepend_size(data) {
            for trim in 0..compressed.len() {
                let _ = decompress_size_prepended(&compressed[..trim]);
            }
        }
    }
});
