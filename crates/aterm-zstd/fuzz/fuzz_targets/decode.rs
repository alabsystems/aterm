// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zstandard decoder fuzz target (docs/AUDIT-third-party-2026-10-15.md ZC.4).
//!
//! ## Running
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no ASan runtime): this lane
//! runs on a stock nightly.
//!
//! ```bash
//! cd crates/aterm-zstd
//! cargo +nightly fuzz run decode -- -max_total_time=300 -timeout=10
//! ```
//!
//! Seed the corpus with real frames to start past the magic number:
//! `mkdir -p fuzz/corpus/decode && cp tests/recorded/cli/*.zst fuzz/corpus/decode/`.
//!
//! ## Properties
//!
//! - `Decoder` never panics on any input: every byte string decodes, or
//!   fails with an `io::Error`.
//! - Memory stays bounded: the output read is capped, and the decoder's own
//!   buffers are bounded by the frame's declarations (at most `WINDOW_MAX`);
//!   libFuzzer's `-rss_limit_mb` (2 GiB by default) fails a run past it.
//! - After an error, every later read fails too.
//! - Round trip: the input stored as a raw frame (`frame::write_raw`, with
//!   and without a checksum) decodes to itself.

#![no_main]

use std::io::Read;

use aterm_zstd::Decoder;
use aterm_zstd::frame::{self, Checksum};
use libfuzzer_sys::fuzz_target;

/// Output read per input, at most: enough for many blocks, small enough to
/// keep each run fast.
const OUTPUT_CAP: usize = 4 << 20;

fuzz_target!(|data: &[u8]| {
    // Property 1: arbitrary input decodes or errs, read in uneven pieces.
    let mut decoder = Decoder::with_buffer(data);
    let mut buf = [0u8; 4093];
    let mut total = 0;
    let failed = loop {
        match decoder.read(&mut buf) {
            Ok(0) => break false,
            Ok(n) => {
                total += n;
                if total > OUTPUT_CAP {
                    break false;
                }
            }
            Err(_) => break true,
        }
    };
    // Property 2: an error is final.
    if failed {
        assert!(
            decoder.read(&mut buf).is_err(),
            "a read after an error succeeded"
        );
    }

    // Property 3: the writer's frames round-trip through the decoder.
    for checksum in [Checksum::Xxh64, Checksum::Omitted] {
        let mut zst = Vec::new();
        frame::write_raw(&mut zst, data, checksum).expect("in-memory write");
        let mut out = Vec::new();
        Decoder::with_buffer(&zst[..])
            .read_to_end(&mut out)
            .expect("a raw frame decodes");
        assert_eq!(out, data);
    }
});
