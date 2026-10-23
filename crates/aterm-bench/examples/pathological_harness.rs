// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pathological-input throughput benchmark across five hostile terminal workloads.

use std::fmt::Write as _;
use std::time::Instant;

use aterm_core::terminal::Terminal;

const CORPUS_BYTES: usize = 16 << 20;
const N_ITERS: usize = 7;
const WARMUP: usize = 2;
const ROWS: u16 = 24;
const COLS: u16 = 80;

fn repeat_to_size(unit: &[u8]) -> Vec<u8> {
    assert!(!unit.is_empty());
    let mut out = Vec::with_capacity(CORPUS_BYTES + unit.len());
    while out.len() < CORPUS_BYTES {
        out.extend_from_slice(unit);
    }
    out
}

fn yes_flood() -> Vec<u8> {
    repeat_to_size(b"y\r\n")
}

fn escape_storm() -> Vec<u8> {
    let mut unit = String::new();
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

fn style_churn() -> Vec<u8> {
    let mut unit = String::new();
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

fn wide_unicode() -> Vec<u8> {
    let unit = "漢字テスト混合行 한국어 텍스트 🚀👨\u{200d}👩\u{200d}👧\u{200d}👦🎨 \
                e\u{301}e\u{301}e\u{301} Ω≈ç√∫ 中文寬字符壓力測試 🔥💧🌈\r\n";
    repeat_to_size(unit.as_bytes())
}

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
