// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Brotli decoder fuzz target, and the writer's round trip: every stream it writes for the
//! input decodes back to it.
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no ASan runtime): run on a stock nightly,
//! `cargo +nightly fuzz run decode` from `crates/aterm-brotli`.

#![no_main]

use std::io::Read;

use aterm_brotli::Decoder;
use libfuzzer_sys::fuzz_target;

/// Output read per input, at most: enough for many meta-blocks, small enough to keep each
/// run fast.
const OUTPUT_CAP: usize = 4 << 20;

fuzz_target!(|data: &[u8]| {
    let mut decoder = Decoder::new(data);
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

    let mut br = Vec::new();
    aterm_brotli::write_uncompressed(&mut br, data).expect("in-memory write");
    let mut out = Vec::new();
    Decoder::new(&br[..])
        .read_to_end(&mut out)
        .expect("a written stream decodes");
    assert_eq!(out, data);
});
