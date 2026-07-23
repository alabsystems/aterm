// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Profiling harness: drive `Terminal::process` on SGR-dense input in a tight loop
// so a sampling profiler (e.g. samply) can attribute time within the engine's hot
// path. Build release, then:
//   cargo build -p aterm-bench --example profile_sgr --release
//   samply record ./target/release/examples/profile_sgr 4000
// Arg = iterations (~1 MiB processed per iteration).

use aterm_core::terminal::Terminal;

fn main() {
    // Same SGR-dense unit as the `comparative` bench's `sgr` corpus.
    let unit = b"\x1b[1;38;5;202mfox\x1b[0m \x1b[4;48;5;19mbar\x1b[0m \x1b[7mx\x1b[27m\r\n";
    let mut corpus = Vec::with_capacity(1 << 20);
    while corpus.len() < (1 << 20) {
        corpus.extend_from_slice(unit);
    }

    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000);
    let mut term = Terminal::new(24, 80);
    let mut sink = 0u64;
    for _ in 0..iters {
        term.process(&corpus);
        // Defeat dead-code elimination of the loop body.
        sink = sink.wrapping_add(u64::from(term.cursor().col));
    }
    std::hint::black_box(sink);
    eprintln!("processed ~{iters} MiB of SGR-dense input");
}
