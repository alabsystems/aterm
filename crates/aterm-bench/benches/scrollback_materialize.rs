// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// ROW -> LINE MATERIALIZATION on the tiered-scrollback drain (ROADMAP WS-K).
//
// WHY THIS EXISTS: nothing in the tree measured `DeferredLine::materialize*` at
// all. Every other aterm-bench target builds `Terminal::new`, whose Grid sets
// `scrollback: None` (aterm-grid/src/grid/construct.rs), so
// `GridStorage::stages_evicted_rows()` is false, `reuse_one_scrolled_row` takes
// the no-scrollback branch, and no `DeferredLine` / `Line` / `Rle` is ever
// built. The shipping GUI does the opposite: aterm-gui/src/spawn.rs builds
// `Terminal::with_scrollback(rows, cols, LIVE_SCROLLBACK_RING_LINES,
// Scrollback::with_defaults())`, so once the ring fills, EVERY scrolled line is
// staged as a `DeferredLine` and materialized on drain — one `String`, one
// `Rle<CellAttrs>` (two heap Vecs) and one `push` per cell, inside a term_lock
// hold. This file supplies the missing workload.
//
// THE LANES ARE A REACH PAIR, AND THE PAIR IS THE POINT.
//
//   * `*_ring*` lanes attach a store and scroll far past the ring, so the drain
//     runs and the store fills. `verify_reaches_target` asserts the store
//     really gained lines (materialization ran).
//   * `plain_no_store` feeds the SAME corpus through a bare `Terminal::new`.
//     It has no store, materializes nothing, and must therefore be INSENSITIVE
//     to any change in the materialization path. It is the negative half of the
//     reach guard and a within-run control arm.
//   * `plain_*` asserts the stored lines carry `attrs() == None` — the
//     all-default `Rle` that `Line::from_parts` discards.
//   * `styled_ring_hot` asserts the stored lines carry `attrs() == Some` with
//     >= 3 runs — the `Rle` that is actually KEPT, so the retained shape is
//     measured too and a change that only helps the discarded case cannot hide
//     a regression on the kept one.
//
// TWO STORE SHAPES, DELIBERATELY. `plain_ring` uses the SHIPPING store config
// (`Scrollback::with_defaults()`: 1000 hot lines, then LZ4 block promotion into
// warm), which is the honest end-to-end price. `plain_ring_hot` raises the hot
// limit above the whole corpus so no block is ever compressed; the per-line
// materialization is then a much larger share of the arm. Reporting both keeps
// the isolating lane from being mistaken for the shipping number.
//
//   cargo bench -p aterm-bench --bench scrollback_materialize

use aterm_core::scrollback::{Rle, Scrollback};
use aterm_core::terminal::Terminal;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// 1 MiB per workload, matching `engine_throughput` / `scrolloff_extract`.
const CORPUS_BYTES: usize = 1 << 20;

/// A maximized window on a 4K display.
const ROWS: u16 = 50;
const COLS: u16 = 200;

/// Columns of text per line. A shell-width line: the materialization walk is
/// `row.len()`, and the per-LINE fixed cost (the `Rle`'s two allocations) is
/// what this bench is about, so an 80-column line keeps both terms visible.
const LINE_COLS: usize = 80;

/// Grid ring size. The shipping ring is 10_000 lines (spawn.rs), which the 1 MiB
/// corpus would never outrun; 1000 puts the run into the steady state a long
/// session lives in — `reuse_scrolled_rows` -> `push_row_boxed` -> drain — after
/// the first 1000 lines instead of never.
const RING_LINES: usize = 1000;

/// Hot-tier limit for the isolating lanes: above the corpus line count, so the
/// hot tier never promotes a block and no LZ4 runs inside the timed region.
const HOT_ONLY_LIMIT: usize = 1 << 20;

/// Granularity of the state sampling in `verify_reaches_target`.
const SAMPLE_CHUNK: usize = 8192;

/// `LINE_COLS` columns of plain ASCII plus CRLF, repeated to `CORPUS_BYTES`.
/// Content varies per line so the corpus is not one repeated cache line.
fn plain_corpus() -> Vec<u8> {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 -_/.";
    let mut out = String::with_capacity(CORPUS_BYTES + 128);
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

/// Same width, but four indexed-colour SGR runs per line and a trailing reset,
/// so the materialized `Rle` has several runs and they are NOT default — the
/// shape `Line::from_parts` KEEPS. Indexed colour (not truecolor) on purpose: it
/// stays in the packed cell, so `ScrolledRowExtras` stays empty and the row still
/// takes `materialize_no_extras`, the same function the plain lanes exercise.
fn styled_corpus() -> Vec<u8> {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 -_/.";
    let sgr = ["\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[34m"];
    let seg = LINE_COLS / 4;
    let mut out = String::with_capacity(CORPUS_BYTES + 128);
    let mut n = 0usize;
    while out.len() < CORPUS_BYTES {
        for (s, code) in sgr.iter().enumerate() {
            out.push_str(code);
            for c in 0..seg {
                out.push(alphabet[(n + s * seg + c) % alphabet.len()] as char);
            }
        }
        out.push_str("\x1b[0m\r\n");
        n = n.wrapping_add(7);
    }
    out.into_bytes()
}

/// What the lane asserts about a materialized line's attribute RLE.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrShape {
    /// Every cell default: `from_parts` collapses the `Rle` to `None`.
    Discarded,
    /// Several distinct styles: the `Rle` is stored and read back.
    Retained,
}

/// Which store a lane attaches.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    /// Bare `Terminal::new` — no store, nothing is ever materialized.
    None,
    /// The shipping `Scrollback::with_defaults()` (1000 hot, LZ4 promotion).
    Shipping,
    /// Hot tier larger than the corpus: no promotion, no LZ4 in the timed region.
    HotOnly,
}

struct Workload {
    name: &'static str,
    store: Store,
    shape: AttrShape,
    corpus: Vec<u8>,
}

fn build(w: &Workload) -> Terminal {
    match w.store {
        Store::None => Terminal::new(ROWS, COLS),
        Store::Shipping => {
            Terminal::with_scrollback(ROWS, COLS, RING_LINES, Scrollback::with_defaults())
        }
        Store::HotOnly => Terminal::with_scrollback(
            ROWS,
            COLS,
            RING_LINES,
            Scrollback::new(HOT_ONLY_LIMIT, HOT_ONLY_LIMIT, 512 * 1024 * 1024),
        ),
    }
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "plain_ring",
            store: Store::Shipping,
            shape: AttrShape::Discarded,
            corpus: plain_corpus(),
        },
        Workload {
            name: "plain_ring_hot",
            store: Store::HotOnly,
            shape: AttrShape::Discarded,
            corpus: plain_corpus(),
        },
        Workload {
            name: "styled_ring_hot",
            store: Store::HotOnly,
            shape: AttrShape::Retained,
            corpus: styled_corpus(),
        },
        Workload {
            name: "plain_no_store",
            store: Store::None,
            shape: AttrShape::Discarded,
            corpus: plain_corpus(),
        },
    ]
}

/// Prove each lane really is in the state it claims — and, for the no-store
/// lane, prove it is really in the OPPOSITE state, so the pair brackets the
/// code under test instead of merely bounding it from one side.
fn verify_reaches_target(w: &Workload) {
    let mut term = build(w);
    let mut saw_lines = false;
    for chunk in w.corpus.chunks(SAMPLE_CHUNK) {
        term.process(chunk);
        match term.scrollback() {
            None => assert!(
                w.store == Store::None,
                "{}: a store lane lost its store — nothing materializes",
                w.name
            ),
            Some(sb) => {
                assert!(
                    w.store != Store::None,
                    "{}: the no-store control grew a store — it is no longer a control",
                    w.name
                );
                if sb.line_count() > 0 {
                    saw_lines = true;
                }
            }
        }
    }

    let scrolled = term.grid().total_lines();
    assert!(
        scrolled > usize::from(ROWS),
        "{}: nothing scrolled at all ({scrolled} total lines)",
        w.name
    );

    let Some(sb) = term.scrollback() else {
        // NEGATIVE half of the guard: the control must reach NONE of it.
        assert!(
            w.store == Store::None,
            "{}: expected a store on this lane",
            w.name
        );
        return;
    };

    // POSITIVE half: lines really were staged, drained and materialized.
    assert!(
        saw_lines && sb.line_count() >= RING_LINES,
        "{}: the store holds {} lines — the drain never ran, so \
         `DeferredLine::materialize*` (the code under test) is never reached. \
         A store only ever gains lines through `reuse_scrolled_rows` -> \
         `push_row_boxed` -> `drain_lazy_buffer` -> `into_line_and_body`.",
        w.name,
        sb.line_count()
    );

    // ... and the materialized lines really have the attribute shape the lane
    // is named for. This is what stops a corpus change from silently turning
    // the retained-Rle lane into another discarded-Rle lane (or vice versa).
    let mut checked = 0usize;
    for idx in [0usize, sb.line_count() / 2, sb.line_count().saturating_sub(1)] {
        let Ok(Some(line)) = sb.get_line(idx) else {
            continue;
        };
        if line.is_empty() {
            continue;
        }
        match w.shape {
            AttrShape::Discarded => assert!(
                line.attrs().is_none(),
                "{}: stored line {idx} KEPT an attrs Rle ({:?} runs) — this lane \
                 exists to price the all-default Rle that `from_parts` discards",
                w.name,
                line.attrs().map(Rle::run_count)
            ),
            AttrShape::Retained => {
                let runs = line.attrs().map_or(0, Rle::run_count);
                assert!(
                    runs >= 3,
                    "{}: stored line {idx} has {runs} attr runs — this lane \
                     exists to price the Rle that is actually KEPT, so it must \
                     carry several distinct styles",
                    w.name
                );
            }
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "{}: no stored line could be read back — the shape assertion never ran",
        w.name
    );
}

fn scrollback_materialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrollback_materialize");
    for w in workloads() {
        verify_reaches_target(&w);
        group.throughput(Throughput::Bytes(w.corpus.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w, |b, w| {
            b.iter(|| {
                // Fresh engine per iteration: one iteration is one honest
                // "a program dumps a MiB into a session with history".
                let mut term = build(w);
                term.process(black_box(&w.corpus));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, scrollback_materialize);
criterion_main!(benches);
