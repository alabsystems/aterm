// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The literal-search scan, at the PUBLIC seam.
//!
//! There was no bench over `aterm-search` at all before this one, which is why
//! it exists: `src/bytesearch.rs` replaced `memchr`'s vector scanners with a
//! word-at-a-time implementation, and a hot path deserves a number rather than
//! a claim. Measuring through `SearchIndex` rather than the private scanner
//! keeps the bench pointed at the work the terminal actually does — index
//! lookup, candidate lines, the literal verify, and the column mapping — so a
//! future change to the scanner shows up here in proportion to how much it
//! actually matters.
//!
//! The corpus is terminal output: build logs, ANSI-free source lines and a
//! low-entropy block that gives the verifier a candidate on almost every byte.
//!
//! # Three shapes this bench used to be blind to
//!
//! The first version of this file measured ONE shape — ~90-byte lines, a query
//! present near the FRONT, always forward — and every regression the scanner
//! carried lived outside it. So there are now three more groups, each of which
//! caught a 7-320x loss when it was added:
//!
//! * `long_lines` — 4 KiB and 64 KiB lines. The scrollback index caps a
//!   logical line at 1 MiB, not a screen width (`MAX_SCROLLBACK_LINE_SCAN_BYTES`),
//!   and a `cat` of a minified file or a docker log line reaches that.
//! * `match_at_end` — the same line with the only occurrence at the far end, so
//!   the scan cannot stop early.
//! * `backward` — the reverse direction the find-prev button drives, which no
//!   forward case touches.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use aterm_search::{SearchDirection, SearchIndex};

/// A deterministic pseudo-random source, so every run indexes the same corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// 20k lines of terminal-shaped text.
fn corpus() -> Vec<String> {
    let mut rng = Rng(0x5DEE_CE66_D1E5_C0DE);
    let mut lines = Vec::with_capacity(20_000);
    for i in 0..20_000u32 {
        match i % 4 {
            0 => lines.push(format!(
                "   Compiling aterm-search v0.63.0 (/private/tmp/build/{i})"
            )),
            1 => lines.push(format!(
                "error[E0599]: no method named `pointer` found for enum `Value` at line {i}"
            )),
            2 => {
                // Source-shaped, with a wide vocabulary.
                let mut line = String::new();
                while line.len() < 90 {
                    line.push_str(&format!("tok{} ", rng.next() % 4096));
                }
                lines.push(line);
            }
            _ => {
                // Low entropy: a candidate start on nearly every byte, which is
                // the shape that punishes a naive prefilter-and-verify.
                lines.push("aaaaaaaaab".repeat(9));
            }
        }
    }
    lines
}

/// Lines of `width` bytes, terminal-shaped, with `tail` appended to each so a
/// query can be planted at the very end.
fn wide_corpus(lines: usize, width: usize, tail: &str) -> Vec<String> {
    let unit = "error[E0599]: no method named `value` found for enum `Node` — ";
    (0..lines)
        .map(|_| {
            let mut line = String::with_capacity(width + tail.len());
            while line.len() < width {
                line.push_str(unit);
            }
            line.truncate(width);
            line.push_str(tail);
            line
        })
        .collect()
}

/// Build an index over `lines`.
fn indexed(lines: &[String]) -> SearchIndex {
    let mut index = SearchIndex::with_capacity(lines.len());
    for line in lines {
        index.push_line(line);
    }
    index
}

fn literal_scan(c: &mut Criterion) {
    let lines = corpus();
    let mut index = SearchIndex::with_capacity(lines.len());
    for line in &lines {
        index.push_line(line);
    }

    let mut group = c.benchmark_group("literal_scan");
    // Short and long needles, present and absent, and one that forces the
    // verifier to reject a candidate at nearly every position.
    for (label, query) in [
        ("present_short", "pointer"),
        ("present_long", "no method named `pointer` found for enum"),
        ("absent_short", "zzqx"),
        (
            "absent_long",
            "a needle that appears nowhere in this corpus at all",
        ),
        ("adversarial_overlap", "aaaaaaaaab_"),
        ("single_byte_ish", "E0"),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(label), &query, |b, query| {
            b.iter(|| black_box(index.search_with_positions(black_box(query))).len());
        });
    }
    group.finish();
}

/// Lines far wider than a screen — the size the scrollback index actually
/// allows — with the query absent, present at the front, and present at the
/// very END so the scan has to cross the whole line.
fn long_lines(c: &mut Criterion) {
    let mut group = c.benchmark_group("long_lines");
    for width in [4096usize, 65_536] {
        let lines = wide_corpus(if width > 8192 { 40 } else { 500 }, width, "__sentinel__");
        let index = indexed(&lines);
        for (label, query) in [
            ("absent", "zzqxvvjjkk"),
            ("present_front", "pointer_free_start"),
            ("match_at_end", "__sentinel__"),
        ] {
            // `present_front` needs to actually be present at the front.
            let query = if label == "present_front" {
                "error[E0599]"
            } else {
                query
            };
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{width}/{label}")),
                &query,
                |b, query| {
                    b.iter(|| black_box(index.search_with_positions(black_box(query))).len());
                },
            );
        }
    }
    group.finish();
}

/// The REVERSE direction, which the find-prev button drives and which no
/// forward case above touches. `many_matches` is the shape that was quadratic:
/// one line holding hundreds of occurrences, walked right to left.
fn backward(c: &mut Criterion) {
    let mut group = c.benchmark_group("backward");
    let ordinary: Vec<String> = (0..300).map(|_| "ab".repeat(100)).collect();
    let dense: Vec<String> = (0..40).map(|_| "abcd".repeat(1024)).collect();
    let cases: [(&str, &Vec<String>, &str); 3] = [
        ("ordinary_200B", &ordinary, "ab"),
        ("dense_4KiB", &dense, "abcd"),
        ("dense_4KiB_absent", &dense, "zzqxvvjjkk"),
    ];
    for (label, lines, query) in cases {
        let index = indexed(lines);
        group.bench_with_input(BenchmarkId::from_parameter(label), &query, |b, query| {
            b.iter(|| {
                black_box(
                    index
                        .search_with_positions_opts_direction(
                            black_box(query),
                            true,
                            false,
                            SearchDirection::Backward,
                        )
                        .map(|found| found.len()),
                )
            });
        });
    }
    group.finish();
}

criterion_group!(benches, literal_scan, long_lines, backward);
criterion_main!(benches);
