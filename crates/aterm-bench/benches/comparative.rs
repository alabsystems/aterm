// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Comparative engine throughput: aterm vs real competitor engines, in-process,
// on identical corpora. This is the honest, reproducible way to answer "is
// aterm's engine faster?" without a display.
//
// WHAT IS FAIR:
//   - `aterm` (full engine: VT parser + grid + state, NO rendering) vs
//     `alacritty_terminal::Term` (Alacritty's full engine: parser + grid +
//     state, NO rendering). Apples-to-apples — both maintain a real screen.
//   - `vte` is Alacritty's *parser only* (no grid) driven by a no-op sink. It is
//     a FLOOR/reference, not a peer of the full engines; labelled accordingly.
//
// WHAT IS NOT MEASURED HERE: GPU rendering / input-to-photon. A full GUI
// terminal (Ghostty/Kitty/WezTerm/Alacritty.app) includes rendering this bench
// deliberately excludes — comparing engine-only to a rendering terminal would
// be dishonest. Run with:
//   cargo bench -p aterm-bench --bench comparative

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const CORPUS_BYTES: usize = 1 << 20; // 1 MiB per workload

/// Deterministic ~1 MiB corpus, identical bytes fed to every engine.
fn corpus(kind: &str) -> Vec<u8> {
    let unit: Vec<u8> = match kind {
        "ascii" => b"the quick brown fox jumps over the lazy dog 0123456789\r\n".to_vec(),
        "sgr" => b"\x1b[1;38;5;202mfox\x1b[0m \x1b[4;48;5;19mbar\x1b[0m \x1b[7mx\x1b[27m\r\n".to_vec(),
        "cjk" => "日本語のテキストをここに置く、端末エンジンの処理速度を測る。\r\n"
            .as_bytes()
            .to_vec(),
        // Realistic shell session: prompt + coloured ls + plain text + a little SGR.
        "mixed" => b"\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/src\x1b[0m$ cargo build\r\n   \x1b[32mCompiling\x1b[0m aterm-core v0.1.0\r\n    Finished in 3.7s\r\n".to_vec(),
        _ => unreachable!(),
    };
    let mut out = Vec::with_capacity(CORPUS_BYTES + unit.len());
    while out.len() < CORPUS_BYTES {
        out.extend_from_slice(&unit);
    }
    out
}

/// 24x80 viewport, no extra history, for alacritty_terminal's grid.
struct Dims;
impl alacritty_terminal::grid::Dimensions for Dims {
    fn total_lines(&self) -> usize {
        24
    }
    fn screen_lines(&self) -> usize {
        24
    }
    fn columns(&self) -> usize {
        80
    }
}

fn comparative(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparative");
    for kind in ["ascii", "sgr", "cjk", "mixed"] {
        let data = corpus(kind);
        group.throughput(Throughput::Bytes(data.len() as u64));

        // --- aterm: full engine (parser + grid + state) ---
        group.bench_with_input(BenchmarkId::new("aterm", kind), &data, |b, data| {
            let mut term = aterm_core::terminal::Terminal::new(24, 80);
            b.iter(|| {
                term.process(black_box(data));
            });
        });

        // --- alacritty_terminal: full engine (parser + grid + state) ---
        group.bench_with_input(BenchmarkId::new("alacritty", kind), &data, |b, data| {
            use alacritty_terminal::Term;
            use alacritty_terminal::event::VoidListener;
            use alacritty_terminal::term::Config;
            use alacritty_terminal::vte::ansi::Processor;
            let mut term = Term::new(Config::default(), &Dims, VoidListener);
            // Pin the defaulted Timeout type param (StdSyncHandler).
            let mut parser: Processor = Processor::new();
            b.iter(|| {
                parser.advance(&mut term, black_box(data));
            });
        });

        // --- vte: Alacritty's parser ONLY, no-op sink (floor / reference) ---
        group.bench_with_input(
            BenchmarkId::new("vte-parser-only", kind),
            &data,
            |b, data| {
                let mut parser = vte::Parser::new();
                let mut sink = NullPerform;
                b.iter(|| {
                    parser.advance(&mut sink, black_box(data));
                });
            },
        );

        // --- termwiz: WezTerm's parser ONLY, no-op callback (floor / reference) ---
        // WezTerm's full engine (wezterm-term) is not published on crates.io, so
        // only its parser layer is comparable in-process — a floor, like vte.
        group.bench_with_input(
            BenchmarkId::new("termwiz-parser-only", kind),
            &data,
            |b, data| {
                let mut parser = termwiz::escape::parser::Parser::new();
                b.iter(|| {
                    parser.parse(black_box(data), |_action| {});
                });
            },
        );
    }
    group.finish();
}

/// No-op vte sink: isolates raw parser-state-machine cost.
struct NullPerform;
impl vte::Perform for NullPerform {
    fn print(&mut self, _c: char) {}
    fn execute(&mut self, _b: u8) {}
    fn hook(&mut self, _p: &vte::Params, _i: &[u8], _ig: bool, _a: char) {}
    fn put(&mut self, _b: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _p: &[&[u8]], _bt: bool) {}
    fn csi_dispatch(&mut self, _p: &vte::Params, _i: &[u8], _ig: bool, _a: char) {}
    fn esc_dispatch(&mut self, _i: &[u8], _ig: bool, _b: u8) {}
}

criterion_group!(benches, comparative);
criterion_main!(benches);
