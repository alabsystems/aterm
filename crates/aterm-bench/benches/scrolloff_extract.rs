// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Scroll-off extras EXTRACTION on PLAIN rows (ROADMAP WS-K, ATERM_DESIGN §7).
//
// WHY THIS EXISTS: `Grid::extract_row_extras_into` runs once per row that leaves
// the screen — `grow_scrollback_ring` while the ring fills, then
// `reuse_scrolled_rows_general` on every newline afterwards — and its own
// comment calls the work "cheap when the CellExtras is empty — common for plain
// text". That is true only while its gate,
//
//     if !extras.has_any_data() && !row.has_style_id() { return; }
//
// stays false. `has_any_data()` is GRID-GLOBAL and STICKY: it is
// `!data.is_empty() || complex_ring.is_some() || rgb_ring.is_some()`, and the
// two dense rings are allocated on the first truecolor / non-BMP write ANYWHERE
// and are never freed. So one truecolor run — a starship/powerlevel10k prompt,
// `ls --color` on a 24-bit theme, a single emoji — signs every plain row that
// scrolls off for the REST OF THE SESSION up to the full per-cell pass: a
// count-and-reserve walk plus a per-cell walk doing `is_spacer`, four flag
// tests and an `extras.get(coord)` FxHashMap probe, inside the PTY reader's
// `term_lock` hold. A 100k-line 200-column flood pays ~20M of those probes for
// a map that is EMPTY.
//
// NO EXISTING TARGET ARMS THAT GATE. `engine_throughput`'s ascii/sgr/cjk corpora
// never write truecolor or non-BMP, so `has_any_data()` stays false and every
// one of them measures the fast path; `end_to_end`'s 64 KiB corpus is indexed
// colour only, same result; `hyperlink_screen` fills the MAP, which is a
// different lane again (the map-empty case is the one the gate gets wrong).
// This file supplies the missing workload.
//
// THE MEASUREMENT IS A PAIR, AND THE PAIR IS THE POINT. Both lanes feed the
// SAME plain-ASCII corpus into the same geometry; the armed lane prepends 16
// columns of truecolor and nothing else. Any difference between them is the
// sticky gate and nothing but the sticky gate — no corpus difference to argue
// about, 16 bytes of prelude out of a MiB. `cold` is therefore both the control
// and the target the fix has to converge on.
//
// WHY AN ASCII **RUN** IN THE PRELUDE, NOT ONE CHARACTER. A truecolor run takes
// `print_ascii_bulk` -> `write_ascii_bulk_with_extras` -> `set_range_uniform`,
// which for RGB-only extras writes the DENSE RING and leaves the map empty — the
// exact state this bench must reproduce. A single truecolor character can fall to
// the per-character path (`apply_cell_extras_preflagged`), which lands a real
// HashMap entry and would quietly convert the armed lane into a map-populated
// workload. `verify_reaches_target` asserts the map stays empty at EVERY sample
// so that substitution cannot happen silently.
//
//   cargo bench -p aterm-bench --bench scrolloff_extract

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// 1 MiB per workload, matching `engine_throughput` and `hyperlink_screen`.
const CORPUS_BYTES: usize = 1 << 20;

/// A maximized window on a 4K display — the geometry where an O(cols) pass per
/// scrolled row actually costs something.
const ROWS: u16 = 50;
const COLS: u16 = 200;

/// Columns of text per line. Wide but plausible: a build log, `cat` of a source
/// file, `tail -f` of a structured log. The extraction pass walks `row.len()`,
/// NOT `cols`, so a corpus of short lines would measure a shorter walk than the
/// one the defect is about.
const LINE_COLS: usize = 160;

/// Granularity of the state sampling in `verify_reaches_target`.
const SAMPLE_CHUNK: usize = 4096;

/// 16 columns of truecolor ASCII, then SGR reset. See the header: a RUN, so the
/// colour lands in the dense RGB ring and the extras map stays empty.
const TRUECOLOR_PRELUDE: &str = "\x1b[38;2;1;2;3m################\x1b[0m\r\n";

/// `LINE_COLS` columns of plain ASCII plus CRLF, repeated to `CORPUS_BYTES`,
/// optionally behind a truecolor prelude. Content varies per line so the corpus
/// is not one repeated cache line.
fn plain_flood(prelude: Option<&str>) -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 64);
    if let Some(p) = prelude {
        out.push_str(p);
    }
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 -_/.";
    let mut n = 0usize;
    while out.len() < CORPUS_BYTES {
        for c in 0..LINE_COLS {
            out.push(alphabet[(n + c) % alphabet.len()] as char);
        }
        out.push_str("\r\n");
        n = n.wrapping_add(7);
    }
    out.into_bytes()
}

struct Workload {
    name: &'static str,
    /// Whether the corpus arms the sticky `has_any_data()` gate.
    armed: bool,
    corpus: Vec<u8>,
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "plain_flood_cold",
            armed: false,
            corpus: plain_flood(None),
        },
        Workload {
            name: "plain_flood_ring_armed",
            armed: true,
            corpus: plain_flood(Some(TRUECOLOR_PRELUDE)),
        },
    ]
}

/// Prove each lane really is in the state it claims, at EVERY sample — not just
/// at the end, where a compaction could have quietly emptied something.
///
/// Two-sided by construction: `armed` asserts the ring IS allocated, `cold`
/// asserts it is NOT, and BOTH assert the extras map stays empty (the map is the
/// other input to the same gate, and a populated map would put both lanes on the
/// slow path and make the pair measure nothing). The scroll assertion is the
/// third leg: a corpus that never scrolls never calls the code under test at all.
fn verify_reaches_target(w: &Workload) {
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    let mut chunks = 0usize;
    for chunk in w.corpus.chunks(SAMPLE_CHUNK) {
        term.process(chunk);
        chunks += 1;
        let extras = term.grid().extras();
        assert!(
            extras.is_empty(),
            "{}: the extras MAP gained an entry — this workload is only \
             meaningful while the map is empty and the sticky RING is the sole \
             reason the gate opens. Did the truecolor prelude stop taking the \
             bulk ASCII run path?",
            w.name
        );
        // The prelude is in the first chunk, so from the second sample on the
        // ring state is settled for the rest of the run.
        if chunks > 1 {
            assert_eq!(
                extras.has_any_data(),
                w.armed,
                "{}: has_any_data() is {} — the lane is not in its intended \
                 state, so the pair no longer isolates the sticky gate",
                w.name,
                extras.has_any_data()
            );
        }
    }
    assert!(
        term.grid().total_lines() > usize::from(ROWS),
        "{}: nothing scrolled off ({} total lines) — `extract_row_extras` is \
         never reached and this workload measures nothing",
        w.name,
        term.grid().total_lines()
    );
}

fn scrolloff_extract(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrolloff_extract");
    for w in workloads() {
        verify_reaches_target(&w);
        group.throughput(Throughput::Bytes(w.corpus.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w, |b, w| {
            b.iter(|| {
                // Fresh engine per iteration, as engine_throughput does: one
                // iteration is one honest "a program dumps a MiB at you".
                let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
                term.process(black_box(&w.corpus));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, scrolloff_extract);
criterion_main!(benches);
