// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte arm's C cut (`docs/DESIGN-card-engine-2026-10-19.md` §3.6), timed:
//! `verify perf throughput`'s corpus through the parse stage's batches and
//! slices without the cut and with it, then the same corpus with a signed
//! OSC 133 C after the first newline past every 4 KiB. Median MB/s of seven
//! passes each, alternating. A measurement for a person, never a gate: the cut's
//! offsets are ASSERTED by `holder_image`'s tests.
//!
//! `targo --unverified bench -p aterm-gui --features bench-support --bench c_cut_cost`

/// The gather's batch (`spawn.rs`'s 64 KiB ring buffers).
const BATCH: usize = 64 * 1024;
/// Timed passes per arm.
const RUNS: usize = 7;

fn main() {
    let corpus = aterm_bench::throughput_corpus(32 << 20);
    report("throughput corpus", &corpus);
    report("a C every 4 KiB", &with_cs(&corpus, 4096));
}

/// `corpus` with a signed OSC 133 C after the first newline past every `every`
/// bytes, so no C lands inside another sequence.
fn with_cs(corpus: &[u8], every: usize) -> Vec<u8> {
    const C: &[u8] = b"\x1b]133;C;id=0123456789abcdef0123456789abcdef\x07";
    let mut out = Vec::with_capacity(corpus.len() + corpus.len() / every * C.len());
    let mut since = 0;
    for &b in corpus {
        out.push(b);
        since += 1;
        if b == b'\n' && since >= every {
            out.extend_from_slice(C);
            since = 0;
        }
    }
    out
}

fn report(what: &str, corpus: &[u8]) {
    let (off, on, cuts) = aterm_gui::c_cut_cost(corpus, BATCH, RUNS);
    println!(
        "PROBE {what:>18}: {:.1} MB, cut off {off:.0} MB/s, on {on:.0} MB/s ({:+.2}%), {cuts} cuts a pass",
        corpus.len() as f64 / 1e6,
        (on / off - 1.0) * 100.0
    );
}
