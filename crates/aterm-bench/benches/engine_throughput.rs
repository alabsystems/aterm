// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Engine throughput (ROADMAP WS-K, ATERM_DESIGN §7). Measures bytes/s of the
// full VT engine (`Terminal::process`) over five workloads. The "3.6 GiB/s"
// headline is RED until reproduced here; this prints the real number.
//
// ─── Why the two truecolour workloads exist ────────────────────────────────
//
// The `sgr` corpus is a THREE-STYLE loop. Whatever the SGR path caches, that
// corpus hits the cache every time after the first repeat, so it prices the
// cache-HIT arm and nothing else. Every cost that scales with the number of
// DISTINCT renditions a session has seen — an intern miss, a hash insert, a
// table push, a refcount store at a cold index, and any capacity behaviour at
// the far end of that table — is invisible to it, and was invisible to every
// other bench in the tree.
//
// `truecolor_unique` and `truecolor_saturating` close that hole. Both emit one
// GENUINELY DISTINCT `\x1b[38;2;R;G;Bm` per cell (a chafa/timg-style image
// dump, a 24-bit gradient script, `bat`/`delta` on a long file), so no
// rendition-keyed cache can help. They are sized to sit on opposite sides of
// 65 535 — the point at which a u16-indexed per-grid style table can hold no
// more entries — so the pair prices BOTH the per-rendition miss cost and
// whatever the engine does once such a table is full. `verify_truecolor_reach`
// asserts that placement from the corpus bytes, so an innocent edit to the
// generator fails the guard instead of quietly measuring the same thing twice.
//
// Both paint IN PLACE (`CSI H` once per screenful, no `\r\n`): scrolling a
// truecolour screen into scrollback is a real cost, but it is the SCROLL path's
// cost, and mixing it in here would dilute the per-rendition signal these two
// workloads exist to isolate. `ascii` and `cjk` stay as the no-regression
// guards for the printable lanes.

use std::collections::HashSet;
use std::fmt::Write as _;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const CORPUS_BYTES: usize = 1 << 20; // 1 MiB per repeat-built workload

const ROWS: u16 = 24;
const COLS: u16 = 80;
/// Cells in one screenful — the truecolour corpora re-home every this many
/// units so the workload never scrolls. Kept honest against `ROWS`/`COLS` by an
/// assert in `verify_truecolor_reach` (a `const` cannot call `u32::from`).
const CELLS_PER_SCREEN: u32 = 24 * 80;

/// Largest number of entries a `u16`-indexed style table can hold (`u16::MAX`).
const U16_INDEX_CAPACITY: usize = 65_535;

/// Distinct renditions in the BELOW-capacity truecolour corpus (~1.0 MiB).
const TRUECOLOR_UNITS_UNDER: u32 = 55_000;
/// Distinct renditions in the ABOVE-capacity truecolour corpus (~1.5 MiB).
const TRUECOLOR_UNITS_OVER: u32 = 80_000;

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

/// Map a unit index to a truecolour triple.
///
/// `n` splits into three bytes, so the map is injective for every `n` below
/// 2^24 — every unit in either corpus is a rendition no other unit repeats.
fn triple(n: u32) -> (u8, u8, u8) {
    let r = u8::try_from(n >> 16).expect("unit counts stay below 2^24");
    let g = u8::try_from((n >> 8) & 0xFF).expect("masked to a byte");
    let b = u8::try_from(n & 0xFF).expect("masked to a byte");
    (r, g, b)
}

/// A `units`-cell truecolour corpus: one distinct 24-bit foreground per cell,
/// re-homed each screenful so nothing scrolls.
fn truecolor_corpus(units: u32) -> Vec<u8> {
    let cap = usize::try_from(units).expect("unit count fits usize") * 20;
    let mut out = String::with_capacity(cap);
    for n in 0..units {
        if n % CELLS_PER_SCREEN == 0 {
            out.push_str("\x1b[H");
        }
        let (r, g, b) = triple(n);
        write!(out, "\x1b[38;2;{r};{g};{b}mX").expect("String writes are infallible");
    }
    out.into_bytes()
}

/// Count the DISTINCT `38;2;R;G;B` triples present in the corpus bytes.
///
/// Reads the corpus, not engine state: this is what pins the workload to its
/// intended side of the capacity line even if the engine's internals change.
fn distinct_truecolor_triples(corpus: &[u8]) -> usize {
    const NEEDLE: &[u8] = b"\x1b[38;2;";
    let mut seen: HashSet<(u8, u8, u8)> = HashSet::new();
    let mut i = 0usize;
    while i + NEEDLE.len() <= corpus.len() {
        if &corpus[i..i + NEEDLE.len()] != NEEDLE {
            i += 1;
            continue;
        }
        let mut j = i + NEEDLE.len();
        let mut parts = [0u32; 3];
        let mut part = 0usize;
        let mut any_digit = false;
        while j < corpus.len() && part < 3 {
            match corpus[j] {
                d @ b'0'..=b'9' => {
                    parts[part] = parts[part] * 10 + u32::from(d - b'0');
                    any_digit = true;
                    j += 1;
                }
                b';' if any_digit => {
                    part += 1;
                    any_digit = false;
                    j += 1;
                }
                _ => break,
            }
        }
        if part == 2 && any_digit {
            let to_u8 = |v: u32| u8::try_from(v).expect("corpus emits 0..=255 components");
            seen.insert((to_u8(parts[0]), to_u8(parts[1]), to_u8(parts[2])));
        }
        i = j.max(i + 1);
    }
    seen.len()
}

/// TWO-SIDED REACH GUARD for the truecolour pair.
///
/// Side 1 (corpus): every unit is a rendition no other unit repeats, and the
/// two corpora land on OPPOSITE sides of the 65 535 `u16`-index capacity — so
/// one prices the per-rendition miss alone and the other prices what happens
/// past a full table. Either bound failing means the pair has collapsed into
/// two copies of the same measurement.
///
/// Side 2 (engine): the corpus must actually reach the styled write path, and a
/// rendition issued AFTER the above-capacity corpus must still paint its own
/// 24-bit colour. That is the load-bearing observation behind this whole
/// workload: cells carry their colours INLINE (`PackedColors` + the extras RGB
/// ring), so no style table can change what a cell paints — which is exactly
/// why work that scales with distinct renditions is worth pricing and, where
/// possible, deleting.
fn verify_truecolor_reach() {
    assert_eq!(
        CELLS_PER_SCREEN,
        u32::from(ROWS) * u32::from(COLS),
        "CELLS_PER_SCREEN drifted from the benched geometry — the truecolour \
         corpora would scroll, and the workload would stop being a pure \
         per-rendition measurement"
    );

    let under = truecolor_corpus(TRUECOLOR_UNITS_UNDER);
    let over = truecolor_corpus(TRUECOLOR_UNITS_OVER);

    let d_under = distinct_truecolor_triples(&under);
    let d_over = distinct_truecolor_triples(&over);

    assert_eq!(
        d_under,
        usize::try_from(TRUECOLOR_UNITS_UNDER).expect("fits usize"),
        "truecolor_unique: {d_under} distinct renditions for {TRUECOLOR_UNITS_UNDER} cells — \
         the generator is repeating colours, so a rendition cache would absorb the workload"
    );
    assert_eq!(
        d_over,
        usize::try_from(TRUECOLOR_UNITS_OVER).expect("fits usize"),
        "truecolor_saturating: {d_over} distinct renditions for {TRUECOLOR_UNITS_OVER} cells — \
         the generator is repeating colours"
    );
    assert!(
        d_under < U16_INDEX_CAPACITY,
        "truecolor_unique must stay BELOW the {U16_INDEX_CAPACITY}-entry capacity \
         (has {d_under}); it exists to price the per-rendition miss alone"
    );
    assert!(
        d_over > U16_INDEX_CAPACITY,
        "truecolor_saturating must cross the {U16_INDEX_CAPACITY}-entry capacity \
         (has only {d_over}); it exists to price the far end of that table"
    );

    // Engine side: the corpus paints, and a post-corpus rendition is exact.
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    term.process(&over);
    let painted = term
        .render_row(0)
        .first()
        .copied()
        .expect("row 0 has cells");
    let blank_fg = aterm_core::terminal::Terminal::new(ROWS, COLS)
        .implicit_blank_render_cell()
        .fg;
    assert_ne!(
        painted.fg, blank_fg,
        "truecolor_saturating did not colour row 0 — the corpus is not reaching \
         the styled write path"
    );
    term.process(b"\x1b[H\x1b[38;2;123;45;67mQ");
    let after = term
        .render_row(0)
        .first()
        .copied()
        .expect("row 0 has cells");
    assert_eq!(
        (after.ch, after.fg),
        ('Q', [123, 45, 67]),
        "a rendition issued after {TRUECOLOR_UNITS_OVER} distinct ones did not paint its \
         own colour — cells are supposed to carry colour inline, independent of any table"
    );
}

fn engine_throughput(c: &mut Criterion) {
    verify_truecolor_reach();

    let mut group = c.benchmark_group("engine_throughput");
    for kind in ["ascii", "sgr", "cjk"] {
        let data = corpus(kind);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(kind), &data, |b, data| {
            b.iter(|| {
                // Fresh 24x80 engine per iteration; feed the whole corpus.
                let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
                term.process(black_box(data));
                black_box(&term);
            });
        });
    }
    for (kind, units) in [
        ("truecolor_unique", TRUECOLOR_UNITS_UNDER),
        ("truecolor_saturating", TRUECOLOR_UNITS_OVER),
    ] {
        let data = truecolor_corpus(units);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(kind), &data, |b, data| {
            b.iter(|| {
                // Fresh engine per iteration: per-session state that grows with
                // distinct renditions must be paid for inside the measurement,
                // not carried over from the warm-up run.
                let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
                term.process(black_box(data));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, engine_throughput);
criterion_main!(benches);
