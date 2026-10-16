// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Zstandard decode throughput harness: `aterm_zstd::Decoder`, the decoder
// atpkg's installer reads `.tar.zst` through, streamed into a 64 KiB buffer as
// the tar reader takes it (docs/AUDIT-third-party-2026-10-15.md ZC.4).
//
//   targo --unverified run --release -q -p aterm-bench --example zstd_decode_harness
//   -> {"zstd_decode_median_mbps":...,"zstd_decode_min_mbps":...,
//       "zstd_decode_max_mbps":...,"output_bytes":...,"n":...,"warmup":...}
//
// The workload is the decoder's own recorded answers: five of the frames the
// zstd 1.5.7 CLI wrote (crates/aterm-zstd/tests/recorded/cli/, pinned by
// SHA-256 in tests/cli_decode.rs): `-19` from a pipe as the packers write,
// `--ultra -22 --long=27`, `--fast=5`, a 1 KiB window, and a single-segment
// file frame over mixed text, noise and binary. (pzstd's frame is left out: its
// input is one 20 KB run repeated, which decodes as long matches several times
// faster and would set the number alone.) No encoder runs here, so the input
// never drifts with a tool's version. MB/s is decoded bytes per second.

use std::hint::black_box;
use std::io::Read;
use std::time::Instant;

use aterm_zstd::Decoder;

/// The recorded frames, and the length each decodes to.
const FRAMES: [(&[u8], usize); 5] = [
    (
        include_bytes!("../../aterm-zstd/tests/recorded/cli/words-96k.19.zst"),
        98_304,
    ),
    (
        include_bytes!("../../aterm-zstd/tests/recorded/cli/words-96k.ultra22-long27.zst"),
        98_304,
    ),
    (
        include_bytes!("../../aterm-zstd/tests/recorded/cli/words-32k.fast5-nocheck.zst"),
        32_768,
    ),
    (
        include_bytes!("../../aterm-zstd/tests/recorded/cli/words-48k.19-wlog10.zst"),
        49_152,
    ),
    (
        include_bytes!("../../aterm-zstd/tests/recorded/cli/mixed-300k.19-file.zst"),
        307_200,
    ),
];

/// Passes over every frame per timed sample: ~37 MB decoded, long enough
/// that one scheduler hiccup does not dominate a sample.
const PASSES: usize = 64;
const N_ITERS: usize = 7;
const WARMUP: usize = 2;

/// Decodes one frame through a 64 KiB read buffer; returns the bytes out.
fn decode(zst: &[u8], buf: &mut [u8]) -> usize {
    let mut decoder = Decoder::with_buffer(zst);
    let mut total = 0;
    loop {
        match decoder.read(buf) {
            Ok(0) => return total,
            Ok(n) => total += n,
            Err(e) => panic!("zstd_decode_harness: a recorded frame failed to decode: {e}"),
        }
    }
}

fn median(samples: &[f64]) -> f64 {
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn main() {
    let mut buf = vec![0u8; 64 * 1024];
    let per_pass: usize = FRAMES.iter().map(|&(_, len)| len).sum();
    let one_sample = |buf: &mut [u8]| -> f64 {
        let t0 = Instant::now();
        for _ in 0..PASSES {
            for &(zst, len) in &FRAMES {
                let got = decode(black_box(zst), buf);
                assert_eq!(
                    got, len,
                    "zstd_decode_harness: a recorded frame decoded short"
                );
            }
        }
        t0.elapsed().as_secs_f64()
    };

    for _ in 0..WARMUP {
        let _ = one_sample(&mut buf);
    }
    let bytes = (per_pass * PASSES) as f64;
    let mut mbps = Vec::with_capacity(N_ITERS);
    for _ in 0..N_ITERS {
        let secs = one_sample(&mut buf);
        mbps.push(if secs > 0.0 {
            bytes / 1.0e6 / secs
        } else {
            0.0
        });
    }
    let med = median(&mbps);
    let min = mbps.iter().copied().fold(f64::INFINITY, f64::min);
    let max = mbps.iter().copied().fold(0.0, f64::max);
    eprintln!(
        "zstd_decode_harness: {:.1} MB per sample over {} recorded frames, median {med:.0} MB/s \
         (min {min:.0}, max {max:.0})",
        bytes / 1.0e6,
        FRAMES.len(),
    );
    println!(
        "{{\"zstd_decode_median_mbps\":{med:.3},\"zstd_decode_min_mbps\":{min:.3},\
         \"zstd_decode_max_mbps\":{max:.3},\"output_bytes\":{},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        per_pass * PASSES,
    );
}
