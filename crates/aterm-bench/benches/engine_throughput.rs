// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Engine throughput (ROADMAP WS-K, ATERM_DESIGN §7). Measures bytes/s of the
// full VT engine (`Terminal::process`) over three workloads. The "3.6 GiB/s"
// headline is RED until reproduced here; this prints the real number.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const CORPUS_BYTES: usize = 1 << 20; // 1 MiB per workload

/// Build a deterministic ~1 MiB corpus of a given flavour.
fn corpus(kind: &str) -> Vec<u8> {
    let unit: Vec<u8> = match kind {
        // Plain printable ASCII lines (the easy, fastest path).
        "ascii" => b"the quick brown fox jumps over the lazy dog 0123456789\r\n".to_vec(),
        // SGR-dense: lots of colour/style escape sequences (parser-heavy).
        "sgr" => {
            b"\x1b[1;38;5;202mfox\x1b[0m \x1b[4;48;5;19mbar\x1b[0m \x1b[7mx\x1b[27m\r\n".to_vec()
        }
        // CJK: wide graphemes (width + grapheme path).
        "cjk" => "日本語のテキストをここに置く、端末エンジンの処理速度を測る。\r\n"
            .as_bytes()
            .to_vec(),
        _ => unreachable!(),
    };
    let mut out = Vec::with_capacity(CORPUS_BYTES + unit.len());
    while out.len() < CORPUS_BYTES {
        out.extend_from_slice(&unit);
    }
    out
}

fn engine_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_throughput");
    for kind in ["ascii", "sgr", "cjk"] {
        let data = corpus(kind);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(kind), &data, |b, data| {
            b.iter(|| {
                // Fresh 24x80 engine per iteration; feed the whole corpus.
                let mut term = aterm_core::terminal::Terminal::new(24, 80);
                term.process(black_box(data));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, engine_throughput);
criterion_main!(benches);
