// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// RESTORE-BENCH harness (E0): session-restore latency floor. A pane restore in
// the product is "replay a serialized ANSI snapshot into a fresh engine" (the
// aterm-wasm `serialize` contract, itself byte-compatible with the daemon's
// `serialize_ansi`); this harness times exactly that — fresh engine + full
// replay — so P5's incremental-hydration work and any parser regression on the
// restore path have a committed before/after number.
//
//   cargo run --release -q -p aterm-bench --example restore_harness
//   -> {"restore_median_hz":...,"restore_median_mbps":...,"payload_bytes":...,
//       "history_lines":...,"n":...,"warmup":...}
//
// The payload is built with the same grid calls (`get_history_line` +
// `row_ansi_text_screen`) and the same layout as `AtermTerminal::serialize`
// (history text + CRLF, scroll-off LFs, absolute-CUP viewport paint, cursor
// restore); the wasm bench (tools/wasm-bench) times the REAL export end-to-end,
// so this native lane only needs the same workload shape, not the same code.

use std::hint::black_box;
use std::time::Instant;

use aterm_core::terminal::{Terminal, TerminalBuilder};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// History depth of the snapshot. 10k lines ≈ a healthy working session (and
/// ~2x the daemon's current 5k default cap, so the number stays meaningful when
/// P4 forwards the real setting).
const HISTORY_LINES: usize = 10_000;

/// Ring holds source + restored history without truncation, keeping the replay
/// workload identical across iterations.
const RING_LINES: usize = 20_000;

const N_ITERS: usize = 7;
const WARMUP: usize = 2;

/// Deterministic session-shaped fill: shell-ish lines with SGR color runs so
/// the replay exercises the SGR path, not just plain text.
fn fill_corpus() -> Vec<u8> {
    let mut out = Vec::with_capacity(HISTORY_LINES * 64);
    for line in 0..HISTORY_LINES {
        match line % 4 {
            0 => out.extend_from_slice(
                format!(
                    "$ build step {line} \x1b[32mok\x1b[0m in 0.{:03}s\r\n",
                    line % 999
                )
                .as_bytes(),
            ),
            1 => out.extend_from_slice(
                format!("\x1b[1;34msrc/module_{line}.rs\x1b[0m: compiled 12 items\r\n").as_bytes(),
            ),
            2 => out.extend_from_slice(
                format!("warning: unused variable `tmp_{line}` \x1b[33m(w{line})\x1b[0m\r\n")
                    .as_bytes(),
            ),
            _ => out.extend_from_slice(
                format!("{line} plain output line with some padding text 0123456789\r\n")
                    .as_bytes(),
            ),
        }
    }
    out
}

/// Build the replayable snapshot with the exact `AtermTerminal::serialize`
/// layout (see the module header): history + scroll-off + viewport + cursor.
fn snapshot(term: &Terminal) -> Vec<u8> {
    let grid = term.grid();
    let history = grid.scrollback_lines();
    let mut out = String::from("\x1b[0m");
    for i in 0..history {
        let line = grid
            .get_history_line(i)
            .and_then(|l| l.as_str().map(|s| s.trim_end().to_string()))
            .unwrap_or_default();
        out.push_str(&line);
        out.push_str("\r\n");
    }
    if history > 0 {
        out.push_str(&format!("\x1b[{ROWS};1H"));
        for _ in 0..history.min(ROWS as usize - 1) {
            out.push('\n');
        }
    }
    out.push_str("\x1b[H");
    for r in 0..ROWS {
        out.push_str(&format!("\x1b[{};1H\x1b[K", r + 1));
        if let Some(row_ansi) = grid.row_ansi_text_screen(r) {
            out.push_str(&row_ansi);
        }
        out.push_str("\x1b[0m");
    }
    let c = term.cursor();
    out.push_str(&format!(
        "\x1b[{};{}H",
        c.row as usize + 1,
        c.col as usize + 1
    ));
    out.into_bytes()
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
    // Source session: fill, then snapshot ONCE (snapshot cost is the serialize
    // side; this lane fences the RESTORE side).
    let corpus = fill_corpus();
    let mut source = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(RING_LINES)
        .build();
    source.process(black_box(&corpus));
    let payload = snapshot(&source);
    let history = source.grid().scrollback_lines();

    // One timed restore: fresh engine + full replay (the product's cold path).
    let one_restore = || -> f64 {
        let mut fresh = TerminalBuilder::new()
            .size(ROWS, COLS)
            .ring_buffer_size(RING_LINES)
            .build();
        let t0 = Instant::now();
        fresh.process(black_box(&payload));
        let secs = t0.elapsed().as_secs_f64();
        black_box(fresh.grid().scrollback_lines());
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        secs
    };

    for _ in 0..WARMUP {
        let _ = one_restore();
    }
    let mut secs = Vec::with_capacity(N_ITERS);
    for _ in 0..N_ITERS {
        secs.push(one_restore());
    }

    let med_secs = median(&secs);
    let hz = 1.0 / med_secs;
    let mbps = (payload.len() as f64 / 1.0e6) / med_secs;
    eprintln!(
        "restore_harness: {history}-line snapshot ({:.1} KiB) restores in {:.1} ms \
         ({hz:.1} restores/s, {mbps:.0} MB/s replay)",
        payload.len() as f64 / 1024.0,
        med_secs * 1e3,
    );
    println!(
        "{{\"restore_median_hz\":{hz:.3},\"restore_median_mbps\":{mbps:.3},\
         \"payload_bytes\":{},\"history_lines\":{history},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        payload.len(),
    );
}
