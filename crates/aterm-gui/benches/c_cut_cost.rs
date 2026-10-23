// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark for `holder_image::read` and the parse stage's OSC 133 C byte arm.

use aterm_uds::spawnfd::{Child, Stdio, command};

/// The gather's batch (`spawn.rs`'s 64 KiB ring buffers).
const BATCH: usize = 64 * 1024;
/// Timed passes per arm.
const RUNS: usize = 7;
/// Timed image reads per process.
const READS: usize = 4000;

fn main() {
    let mut zsh = blocked("/bin/zsh", &["-f"]);
    let mut sh = blocked("/bin/sh", &["/dev/stdin"]);
    let mut cat = blocked("/bin/cat", &[]);
    for (what, child) in [("zsh -f", &zsh), ("sh /dev/stdin", &sh), ("cat", &cat)] {
        for (how, kept) in [("fresh", false), ("kept", true)] {
            let (p50, p95, p99, image) = aterm_gui::holder_read_cost(pid(child), READS, kept);
            println!(
                "PROBE read {what:>14} {how:>5}: µs/read p50 {p50:.2} p95 {p95:.2} p99 {p99:.2} ({image})"
            );
        }
    }
    let corpus = aterm_bench::throughput_corpus(32 << 20);
    let with_c = with_cs(&corpus, 4096);
    report("throughput corpus", &corpus, 0);
    report("a C every 4 KiB", &with_c, 0);
    report("corpus, zsh unresolved", &corpus, pid(&zsh));
    report("C/4 KiB, zsh unresolved", &with_c, pid(&zsh));
    for child in [&mut zsh, &mut sh, &mut cat] {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// `program args` with its stdin a pipe this bench keeps open.
fn blocked(program: &str, args: &[&str]) -> Child {
    command(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {program}: {e}"))
}

fn pid(child: &Child) -> i32 {
    i32::try_from(child.id()).expect("a pid")
}

/// `corpus` with a signed OSC 133 C after the first newline past every `every` bytes.
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

fn report(what: &str, corpus: &[u8], holder: i32) {
    let (off, on, cuts, reads) = aterm_gui::c_cut_cost(corpus, BATCH, RUNS, holder);
    println!(
        "PROBE {what:>23}: {:.1} MB, arm off {off:.0} MB/s, on {on:.0} MB/s ({:+.2}%), {cuts} cuts and {reads} image reads a pass",
        corpus.len() as f64 / 1e6,
        (on / off - 1.0) * 100.0
    );
}
