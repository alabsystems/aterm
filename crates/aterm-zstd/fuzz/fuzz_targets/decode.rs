// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zstandard decoder fuzz target (docs/AUDIT-third-party-2026-10-15.md ZC.4).
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no ASan runtime): run on a stock nightly,
//! `cargo +nightly fuzz run decode` from `crates/aterm-zstd`.

#![no_main]

use std::io::Read;

use aterm_zstd::Decoder;
use aterm_zstd::frame::{self, Checksum};
use libfuzzer_sys::fuzz_target;

/// Output read per input, at most: enough for many blocks, small enough to
/// keep each run fast.
const OUTPUT_CAP: usize = 4 << 20;

fuzz_target!(|data: &[u8]| {
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
    if failed {
        assert!(
            decoder.read(&mut buf).is_err(),
            "a read after an error succeeded"
        );
    }

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
