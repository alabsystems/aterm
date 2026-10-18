// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XXH64 against 577 answers recorded from the xxHash reference C code
//! (`recorded/xxh64-libzstd-1.5.7.tsv` says how): every length through two
//! stripes and the 4- and 8-byte tails, a 1 MiB input, and seeds that wrap
//! the accumulators.

mod support;

use aterm_zstd::xxh64::xxh64;
use support::sanity;

const RECORDED: &str = include_str!("recorded/xxh64-libzstd-1.5.7.tsv");

/// One recorded row: seed, input length, hash.
#[derive(Debug)]
struct Row {
    seed: u64,
    len: usize,
    want: u64,
}

fn rows() -> Vec<Row> {
    let hex = |s: &str| u64::from_str_radix(s, 16).expect("hex");
    RECORDED
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 3, "malformed row: {l}");
            Row {
                seed: hex(f[0]),
                len: f[1].parse().expect("length"),
                want: hex(f[2]),
            }
        })
        .collect()
}

fn longest(rows: &[Row]) -> usize {
    rows.iter().map(|r| r.len).max().expect("rows")
}

#[test]
fn every_recorded_answer() {
    let rows = rows();
    assert_eq!(rows.len(), 577);
    let buf = sanity(longest(&rows));
    for r in &rows {
        assert_eq!(
            xxh64(r.seed, &buf[..r.len]),
            r.want,
            "seed {:#x} len {}",
            r.seed,
            r.len
        );
    }
}
