// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LZ4 decompression fuzz target.
//!
//! This fuzzer tests the inline LZ4 block decompressor with arbitrary byte
//! sequences, verifying that it never panics and that round-trips are
//! faithful.
//!
//! ## Running
//!
//! ```bash
//! cd crates/aterm-scrollback
//! cargo +nightly fuzz run lz4_decompress -- -max_total_time=60 -timeout=10
//! ```
//!
//! ## Properties Tested
//!
//! - `decompress_size_prepended` never panics on any input (returns `Err` for
//!   malformed data)
//! - Round-trip: `decompress(compress(data)) == data` for all valid inputs
//! - Allocation bomb protection: oversized size prefixes are rejected
//! - No memory corruption from overlapping back-references

#![no_main]

use libfuzzer_sys::fuzz_target;

use aterm_scrollback::lz4::{compress_prepend_size, decompress_size_prepended};

fuzz_target!(|data: &[u8]| {
    // -----------------------------------------------------------------------
    // Property 1: decompress never panics on arbitrary input.
    //
    // The decompressor must return Ok or Err for any byte sequence. This is
    // security-critical because scrollback pages stored on disk could be
    // corrupted or tampered with.
    // -----------------------------------------------------------------------
    let _ = decompress_size_prepended(data);

    // -----------------------------------------------------------------------
    // Property 2: round-trip fidelity.
    //
    // Use the fuzzer input as "original data" to compress, then decompress
    // and verify the output matches. Cap the input length to avoid spending
    // all fuzzer time on huge allocations -- 256 KiB is well above any real
    // scrollback page size.
    // -----------------------------------------------------------------------
    if data.len() <= 256 * 1024 {
        if let Ok(compressed) = compress_prepend_size(data) {
            let decompressed = decompress_size_prepended(&compressed)
                .expect("round-trip: decompression of our own compressed data must succeed");
            assert_eq!(
                decompressed, data,
                "round-trip: decompressed output differs from original input ({} bytes)",
                data.len()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Property 3: allocation bomb rejection.
    //
    // Craft a 4-byte size prefix claiming a huge decompressed size followed
    // by the fuzzer payload as the "block". The decompressor must reject
    // sizes above the 16 MiB safety cap without allocating.
    // -----------------------------------------------------------------------
    if data.len() >= 4 {
        // Inject an oversized size prefix (17 MiB) before the rest of the data.
        let bomb_size: u32 = 17 * 1024 * 1024;
        let mut bomb_input = bomb_size.to_le_bytes().to_vec();
        bomb_input.extend_from_slice(&data[4..]);
        let result = decompress_size_prepended(&bomb_input);
        assert!(
            result.is_err(),
            "allocation bomb: 17 MiB size prefix must be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // Property 4: truncated compressed data is handled gracefully.
    //
    // Take valid compressed output and truncate it at every possible byte
    // offset. The decompressor must not panic on any truncation. Only do
    // this for small inputs to keep the fuzzer fast.
    // -----------------------------------------------------------------------
    if data.len() <= 512 {
        if let Ok(compressed) = compress_prepend_size(data) {
            for trim in 0..compressed.len() {
                let _ = decompress_size_prepended(&compressed[..trim]);
            }
        }
    }
});
