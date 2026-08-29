// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE AGENT-FACING SCREEN READ — the introspection path nothing priced.
//
// `frame_latency` prices the render path, `workspace_scaling` the whole-workspace
// passes around it, `subscribe_digest` the `events` stream. None of them touches
// the STYLED FRAME: `aterm ctl screen` and every `subscribe … cells` push, which
// is how an agent reads the screen and therefore the single control verb most
// likely to be called in a loop.
//
// WHAT THIS FILE IS FOR. The frame is gathered with the terminal MUTEX HELD (one
// acquisition, so every field describes the same instant) and serialized with it
// RELEASED. Everything the gather does is therefore paid by the engine as well as
// by the caller: keystroke encode, the PTY drain and a frame snapshot all queue
// behind it. So the two phases are timed SEPARATELY — a single "screen verb"
// number could not say which side a change moved, and only one of the two sides
// blocks the terminal.
//
// WHAT IS TIMED, EXACTLY — the `frame_latency` contract, verbatim:
//
//     let term = painted_screen(..);   // UNTIMED: fixture paint through the parser
//     let t0 = Instant::now();         //  ── timed span opens
//     let g = black_box(GatheredFrame::gather(&term));   // the SHIPPING call
//     total += t0.elapsed();           //  ── timed span closes
//     drop(g);                         //  frees land OUTSIDE the span
//
// The drop is deliberately outside: in production the snapshot is released after
// serialization, off the lock, so charging its frees to the in-lock phase would
// overstate exactly the number this bench exists to report.
//
// THE AXES. Grid size (24x80, the agent default; 50x200, a large window) crossed
// with content shape (dense ASCII; CJK + combining clusters). Content is an axis
// because the per-cell cost is dominated by GRAPHEME EXTRACTION, and a wide row's
// graphemes are three bytes with an empty continuation between them.
//
// THE PAIRED A/B (`styled_frame_glyphs`). The gather's per-cell cost was
// dominated by GRAPHEME EXTRACTION, and that phase is priced here in BOTH shapes,
// in ONE binary, back to back: `per_cell` is the retired
// `Terminal::cell_grapheme` once per cell (a `String` per cell, `String::new()`
// for the continuations that own no glyph), `row_buffer` is the shipping
// `cell_grapheme_into` appending into one buffer per row with a byte range per
// cell. Same grid, same reads, same process — the only difference is where the
// bytes land, which is the whole of the change. A two-build A/B could not say
// that much: it would carry the whole binary's codegen drift with it.
//
// The arms are also checked to produce IDENTICAL glyphs before either is timed,
// so the cheap arm cannot be cheap by doing less.
//
// TWO-SIDED GUARDS, on every workload:
//
//   * REACH (lower): the gather really produced `rows × cols` cells — the frame's
//     lossless no-trim contract — and the serialized frame really carries the
//     painted glyph. A fixture that failed to paint would otherwise be priced as
//     a very fast screen read of an empty grid.
//
//   * BOUND (upper): fewer than half the cells serialize as a blank glyph, so the
//     screen under test is genuinely full. The blank path is the cheapest one
//     through the gather, and a workload that quietly drifted into it would look
//     like a win.

use std::time::{Duration, Instant};

use aterm_gui::bench_support::{GatheredFrame, ScreenFill, painted_screen};
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ dials --

/// The grid sweep. 24x80 is the default agent-facing session; 50x200 is a large
/// window, where the same per-cell work is done 10,000 times per read.
const GRIDS: [(u16, u16); 2] = [(24, 80), (50, 200)];

// ------------------------------------------------------------------ report --

/// The one human-readable line each verify pass prints: what state the workload
/// reached, in the numbers its guards assert on.
fn report(name: &str, detail: &str) {
    println!("REACH {name:<30} | {detail}");
}

// ------------------------------------------------------------------ verify --

/// PROVE one screen workload before it is timed, then hand the painted terminal
/// back so the timed run reads exactly the state that was verified.
fn verify(rows: u16, cols: u16, fill: ScreenFill, name: &str) -> aterm_core::terminal::Terminal {
    let term = painted_screen(rows, cols, fill);
    let cells = usize::from(rows) * usize::from(cols);

    let gathered = GatheredFrame::gather(&term);
    assert_eq!(
        gathered.cells(),
        cells,
        "{name}: the gather produced {} cells, not the {cells} of the lossless \
         rows x cols contract — this arm is not pricing a full screen",
        gathered.cells()
    );

    let frame = gathered.serialize();
    let painted = match fill {
        ScreenFill::Ascii => "\"glyph\":\"a\"",
        ScreenFill::Wide => "\"glyph\":\"界\"",
    };
    assert!(
        frame.contains(painted),
        "{name}: no {painted} anywhere in the serialized frame — the fixture did \
         not paint, and this arm would price an empty grid"
    );
    let blanks = frame.matches("\"glyph\":\" \"").count();
    assert!(
        blanks * 2 < cells,
        "{name}: {blanks} of {cells} cells serialize blank — the screen under \
         test is mostly empty, which is the cheapest path through the gather"
    );

    report(
        name,
        &format!(
            "cells={cells} blank={blanks} frame={}B ({} B/cell)",
            frame.len(),
            frame.len() / cells
        ),
    );
    term
}

// ------------------------------------------------------------------ timing --

/// The IN-LOCK GATHER: the phase that blocks the engine. This is the number the
/// finding is about — every allocation here is one the terminal mutex waits for.
fn gather(g: &mut BenchmarkGroup<'_, WallTime>, rows: u16, cols: u16, fill: ScreenFill) {
    let id = format!("{rows}x{cols}_{}", fill_name(fill));
    let term = verify(rows, cols, fill, &format!("gather/{id}"));
    g.bench_function(BenchmarkId::from_parameter(&id), |bch| {
        bch.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let t0 = Instant::now();
                let gathered = black_box(GatheredFrame::gather(black_box(&term)));
                total += t0.elapsed();
                // Outside the span on purpose: production drops the snapshot
                // after serialization, with the lock released.
                drop(gathered);
            }
            total
        });
    });
}

/// The OFF-LOCK SERIALIZE, priced beside the gather so a change that moved work
/// from one phase into the other cannot read as a win in isolation.
fn serialize(g: &mut BenchmarkGroup<'_, WallTime>, rows: u16, cols: u16, fill: ScreenFill) {
    let id = format!("{rows}x{cols}_{}", fill_name(fill));
    let term = verify(rows, cols, fill, &format!("serialize/{id}"));
    let gathered = GatheredFrame::gather(&term);
    g.bench_function(BenchmarkId::from_parameter(&id), |bch| {
        bch.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let t0 = Instant::now();
                let json = black_box(gathered.serialize());
                total += t0.elapsed();
                drop(json);
            }
            total
        });
    });
}

// ------------------------------------------------------ the paired A/B --

/// The RETIRED glyph extraction: one `String` per cell, exactly as the gather
/// spelled it (`t.cell_grapheme(r, c).unwrap_or_default()`).
fn per_cell_glyphs(term: &aterm_core::terminal::Terminal, rows: u16, cols: u16) -> Vec<String> {
    let mut out = Vec::with_capacity(usize::from(rows) * usize::from(cols));
    for r in 0..usize::from(rows) {
        for c in 0..usize::from(cols) {
            out.push(term.cell_grapheme(r, c).unwrap_or_default());
        }
    }
    out
}

/// The SHIPPING glyph extraction: one buffer per row, one byte range per cell.
fn row_buffer_glyphs(
    term: &aterm_core::terminal::Terminal,
    rows: u16,
    cols: u16,
) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
    let mut buffers = Vec::with_capacity(usize::from(rows));
    let mut ranges = Vec::with_capacity(usize::from(rows) * usize::from(cols));
    for r in 0..usize::from(rows) {
        let mut glyphs = String::with_capacity(usize::from(cols));
        for c in 0..usize::from(cols) {
            let start = glyphs.len();
            term.cell_grapheme_into(r, c, &mut glyphs);
            ranges.push(start..glyphs.len());
        }
        buffers.push(glyphs);
    }
    (buffers, ranges)
}

/// Both glyph shapes over one painted screen, priced back to back. PROVEN
/// EQUIVALENT first: every cell's bytes must match, or the cheap arm is cheap
/// because it is reading less.
fn glyphs(g: &mut BenchmarkGroup<'_, WallTime>, rows: u16, cols: u16, fill: ScreenFill) {
    let id = format!("{rows}x{cols}_{}", fill_name(fill));
    let term = verify(rows, cols, fill, &format!("glyphs/{id}"));

    let old = per_cell_glyphs(&term, rows, cols);
    let (buffers, ranges) = row_buffer_glyphs(&term, rows, cols);
    assert_eq!(old.len(), ranges.len(), "glyphs/{id}: cell counts differ");
    for (i, want) in old.iter().enumerate() {
        let got = &buffers[i / usize::from(cols)][ranges[i].clone()];
        assert_eq!(
            got, want,
            "glyphs/{id}: the two shapes disagree at cell {i} — this A/B would \
             be pricing two different reads"
        );
    }

    g.bench_function(
        BenchmarkId::from_parameter(format!("per_cell_{id}")),
        |bch| {
            bch.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    let v = black_box(per_cell_glyphs(black_box(&term), rows, cols));
                    total += t0.elapsed();
                    drop(v);
                }
                total
            });
        },
    );
    g.bench_function(
        BenchmarkId::from_parameter(format!("row_buffer_{id}")),
        |bch| {
            bch.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    let v = black_box(row_buffer_glyphs(black_box(&term), rows, cols));
                    total += t0.elapsed();
                    drop(v);
                }
                total
            });
        },
    );
}

fn fill_name(fill: ScreenFill) -> &'static str {
    match fill {
        ScreenFill::Ascii => "ascii",
        ScreenFill::Wide => "wide",
    }
}

fn styled_frame(c: &mut Criterion) {
    {
        let mut g = c.benchmark_group("styled_frame_gather");
        for &(rows, cols) in &GRIDS {
            for fill in [ScreenFill::Ascii, ScreenFill::Wide] {
                gather(&mut g, rows, cols, fill);
            }
        }
        g.finish();
    }
    {
        let mut g = c.benchmark_group("styled_frame_glyphs");
        for &(rows, cols) in &GRIDS {
            for fill in [ScreenFill::Ascii, ScreenFill::Wide] {
                glyphs(&mut g, rows, cols, fill);
            }
        }
        g.finish();
    }
    {
        let mut g = c.benchmark_group("styled_frame_serialize");
        for &(rows, cols) in &GRIDS {
            serialize(&mut g, rows, cols, ScreenFill::Ascii);
        }
        g.finish();
    }
}

criterion_group!(benches, styled_frame);
criterion_main!(benches);
