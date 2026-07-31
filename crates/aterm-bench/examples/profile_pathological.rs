// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Profiling harness for the PATHOLOGICAL-BENCH corpora. `pathological_harness`
// reports one throughput number per corpus but cannot say *where* the time goes;
// this drives ONE named corpus through `Terminal::process` in a tight loop so a
// sampling profiler can attribute it.
//
// The corpus generators below are copied VERBATIM from `pathological_harness`
// and the per-iteration shape is identical (a FRESH `Terminal` per iteration,
// so retained state never accumulates) — otherwise a profile here would explain
// some neighbouring workload rather than the number the gate actually reports.
// Keep the two in sync; `pathological_harness` is the source of truth.
//
//   cargo build -p aterm-bench --example profile_pathological --profile profiling
//   ./target/profiling/examples/profile_pathological yes_flood 40 &
//   sample $! 10 1 -f /tmp/prof.txt
//
// Args: <corpus> [iters]. Corpus is one of the five PATHOLOGICAL_CORPORA names;
// each iteration processes one 16 MiB corpus on a fresh engine.

use std::fmt::Write as _;

use aterm_core::terminal::Terminal;

const CORPUS_BYTES: usize = 16 << 20;
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
    unit.push_str("\x1b[H\x1b[2J");
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

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_else(|| "yes_flood".to_string());
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    let corpus = match name.as_str() {
        "yes_flood" => yes_flood(),
        "escape_storm" => escape_storm(),
        "style_churn" => style_churn(),
        "long_escapes" => long_escapes(),
        "wide_unicode" => wide_unicode(),
        other => {
            eprintln!(
                "unknown corpus {other}; expected one of yes_flood escape_storm style_churn long_escapes wide_unicode"
            );
            std::process::exit(2);
        }
    };

    let mut sink = 0u64;
    for _ in 0..iters {
        // Fresh engine per iteration — identical to `pathological_harness`.
        let mut term = Terminal::new(ROWS, COLS);
        term.process(std::hint::black_box(&corpus));
        sink = sink.wrapping_add(u64::from(term.cursor().col));
    }
    std::hint::black_box(sink);
    eprintln!(
        "profile_pathological: {name} x{iters} ({} MiB total)",
        (corpus.len() * iters) >> 20
    );
}
