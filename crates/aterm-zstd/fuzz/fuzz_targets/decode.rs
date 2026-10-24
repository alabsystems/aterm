// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zstandard decoder fuzz target, and the encoder's round trip: every frame it writes for
//! the input decodes back to it.
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no ASan runtime): run on a stock nightly,
//! `cargo +nightly fuzz run decode` from `crates/aterm-zstd`.

#![no_main]

use std::io::{Read, Write};

use aterm_zstd::Decoder;
use aterm_zstd::encode::{Encoder, LEVEL_MAX, Options};
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

    // The encoder's frames decode to the input: a level, window and checksum choice from
    // the input's first bytes, written in one call and again in writes of a size the input
    // picks, with a flush between some of them.
    let pick = |i: usize| data.get(i).copied().unwrap_or(0);
    let mut options = Options::new().level(1 + pick(0) % LEVEL_MAX);
    if pick(1) & 1 == 1 {
        options = options.window_log(10 + pick(1) % 8);
    }
    if pick(2) & 1 == 1 {
        options = options.checksum(Checksum::Omitted);
    }
    let one_shot = aterm_zstd::compress(data, &options);
    let mut out = Vec::new();
    Decoder::with_buffer(&one_shot[..])
        .read_to_end(&mut out)
        .expect("an encoded frame decodes");
    assert_eq!(out, data, "one-shot frame");

    let step = 1 + usize::from(pick(3)) * 37;
    let mut enc = Encoder::new(Vec::new(), options);
    for (i, piece) in data.chunks(step).enumerate() {
        enc.write_all(piece).expect("in-memory write");
        if i % 3 == 2 {
            enc.flush().expect("in-memory flush");
        }
    }
    let streamed = enc.finish().expect("in-memory finish");
    out.clear();
    Decoder::with_buffer(&streamed[..])
        .read_to_end(&mut out)
        .expect("a streamed frame decodes");
    assert_eq!(out, data, "streamed frame");
});
