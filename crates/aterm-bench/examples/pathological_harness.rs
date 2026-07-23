// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PATHOLOGICAL-BENCH (EXCEED_GHOSTTY_PLAN Cluster G / FASTER_THAN_GHOSTTY_PLAN
// §4 STYLE-1): wall-clock throughput floors under HOSTILE input, the lane the
// mixed-workload `perf_harness` deliberately does not cover. Same measurement
// discipline (release subprocess, deterministic corpora, warmup + median-of-N,
// one flat JSON line on stdout), one floor PER CORPUS so a regression names the
// workload class that caused it.
//
//   cargo run --release -q -p aterm-bench --example pathological_harness
//   -> {"yes_flood_median_mbps":...,...,"n":7,"warmup":2,"corpus_bytes":16777216}
//
// The five corpora and why each is here:
//   yes_flood     `y\r\n` forever — the classic `yes(1)` flood: maximal
//                 line/scroll rate, minimal payload; the scroll path IS the cost.
//   escape_storm  cursor-motion-dense short CSIs (~85% escape bytes) — the
//                 vtebench cursor-motion class; escape-dense TUIs.
//   style_churn   a distinct truecolor fg+bg per cell — the DOOM-fire class
//                 (ghostty optimizes for ~16-64 unique styles BY DESIGN; our
//                 style-interning table must not degrade here).
//   long_escapes  ~1 KiB OSC payloads (hyperlinks, titles) — long-escape
//                 throughput, a publicly-admitted ghostty weak spot; exercises
//                 our bulk OSC scan + the 8 MiB cap discipline.
//   wide_unicode  CJK + ZWJ-emoji + combining marks — the multibyte decode,
//                 width, and grapheme paths under maximal pressure.

use std::fmt::Write as _;
use std::time::Instant;

use aterm_core::terminal::Terminal;

/// Per-corpus size. Half the mixed lane's 32 MiB: five corpora × (2 warmup + 7
/// timed) iterations must stay a sub-minute gate stage even at pathological
/// (slow) throughputs, while one iteration still dwarfs timer granularity.
const CORPUS_BYTES: usize = 16 << 20;

/// Median-of-N + warmup: identical discipline to `perf_harness` (odd N so the
/// median is a single sample; warmup absorbs cache/branch/frequency ramp).
const N_ITERS: usize = 7;
const WARMUP: usize = 2;

/// Same fixed grid as the mixed lane: small, so scrolling dominates like a
/// real shell window.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Repeat `unit` to at least [`CORPUS_BYTES`].
fn repeat_to_size(unit: &[u8]) -> Vec<u8> {
    assert!(!unit.is_empty());
    let mut out = Vec::with_capacity(CORPUS_BYTES + unit.len());
    while out.len() < CORPUS_BYTES {
        out.extend_from_slice(unit);
    }
    out
}

/// `yes(1)` through a PTY with ONLCR: `y\r\n`, forever.
fn yes_flood() -> Vec<u8> {
    repeat_to_size(b"y\r\n")
}

/// Cursor-motion storm: short CSIs vastly outnumbering printable bytes, with
/// enough row churn that damage tracking gets no easy full-screen shortcuts.
fn escape_storm() -> Vec<u8> {
    let mut unit = String::new();
    // 24 hops around the grid per block, one printable byte per hop, plus
    // save/restore + partial erases — the escape-dense TUI shape.
    for i in 0u32..24 {
        let row = 1 + (i * 7) % u32::from(ROWS);
        let col = 1 + (i * 13) % u32::from(COLS);
        let _ = write!(
            unit,
            "\x1b7\x1b[{row};{col}H\x1b[1K*\x1b[{}A\x1b[{}Cx\x1b8",
            1 + i % 8,
            1 + i % 11,
        );
    }
    unit.push_str("\x1b[H\x1b[2J"); // periodic full reset, then the storm again
    repeat_to_size(unit.as_bytes())
}

/// A distinct truecolor fg+bg pair per printed cell (the DOOM-fire shape): the
/// style-interning table sees maximal churn; SGR parse + dispatch dominate.
fn style_churn() -> Vec<u8> {
    let mut unit = String::new();
    // 20 full rows of per-cell restyling per block; the RGB walk has period
    // 256 per channel with co-prime strides, so on-screen style diversity
    // stays in the thousands (far past any ~64-style comfort zone).
    for cell in 0u32..(20 * u32::from(COLS)) {
        let (r, g, b) = ((cell * 7) & 0xff, (cell * 13) & 0xff, (cell * 29) & 0xff);
        let ch = char::from(b'a' + (cell % 26) as u8);
        let _ = write!(unit, "\x1b[38;2;{r};{g};{b}m\x1b[48;2;{b};{r};{g}m{ch}");
        if cell % u32::from(COLS) == u32::from(COLS) - 1 {
            unit.push_str("\x1b[0m\r\n");
        }
    }
    repeat_to_size(unit.as_bytes())
}

/// ~1 KiB OSC payloads: an OSC 8 hyperlink with a long deterministic URL (ST-
/// terminated), link text, close, then a long OSC 0 title (BEL-terminated).
fn long_escapes() -> Vec<u8> {
    let mut url = String::from("https://example.invalid/");
    while url.len() < 900 {
        let _ = write!(url, "seg{:04x}/", url.len());
    }
    let mut title = String::from("pathological-title-");
    while title.len() < 600 {
        let _ = write!(title, "{:03x}", title.len());
    }
    let unit = format!(
        "\x1b]8;;{url}\x1b\\link text under a kilobyte-long target\x1b]8;;\x1b\\ \
         \x1b]0;{title}\x07 plain tail\r\n"
    );
    repeat_to_size(unit.as_bytes())
}

/// Multibyte-dense: wide CJK, ZWJ emoji families, combining marks — every line
/// forces the multibyte decode path, width lookups, and grapheme clustering.
fn wide_unicode() -> Vec<u8> {
    let unit = "漢字テスト混合行 한국어 텍스트 🚀👨\u{200d}👩\u{200d}👧\u{200d}👦🎨 \
                e\u{301}e\u{301}e\u{301} Ω≈ç√∫ 中文寬字符壓力測試 🔥💧🌈\r\n";
    repeat_to_size(unit.as_bytes())
}

/// Process the corpus once on a fresh engine; MB/s (decimal, matching the
/// mixed lane). Fresh engine per iter so retained state never accumulates.
fn one_iter_mbps(corpus: &[u8]) -> f64 {
    let mut term = Terminal::new(ROWS, COLS);
    let t0 = Instant::now();
    term.process(std::hint::black_box(corpus));
    let elapsed = t0.elapsed();
    std::hint::black_box(term.cursor().col);
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return f64::INFINITY;
    }
    (corpus.len() as f64 / 1.0e6) / secs
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
    // NOTE: the corpus NAME LIST is mirrored by `xtask::perf::PATHOLOGICAL_CORPORA`
    // (the gate reads `<name>_median_mbps` for each) — keep the two in sync.
    let corpora: [(&str, Vec<u8>); 5] = [
        ("yes_flood", yes_flood()),
        ("escape_storm", escape_storm()),
        ("style_churn", style_churn()),
        ("long_escapes", long_escapes()),
        ("wide_unicode", wide_unicode()),
    ];

    let mut json = String::from("{");
    for (name, corpus) in &corpora {
        for _ in 0..WARMUP {
            let _ = one_iter_mbps(corpus);
        }
        let mut samples = Vec::with_capacity(N_ITERS);
        for _ in 0..N_ITERS {
            samples.push(one_iter_mbps(corpus));
        }
        let med = median(&samples);
        let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
        let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        eprintln!(
            "pathological_harness: {name}: median {med:.1} MB/s (min {min:.1}, max {max:.1}) \
             over {:.1} MiB x {N_ITERS}",
            corpus.len() as f64 / (1u64 << 20) as f64,
        );
        let _ = write!(json, "\"{name}_median_mbps\":{med:.3},");
    }
    let _ = write!(
        json,
        "\"corpus_bytes\":{CORPUS_BYTES},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    );
    println!("{json}");
}
