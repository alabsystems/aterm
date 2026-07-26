// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// POSTING-CONTAINER decision bench (audit E4 / fed E-3, Wave-4A milestone 3).
//
// The search index keys one posting container per trigram; the shipping
// container is `SparseBitmap` = `BTreeSet<u32>`. This harness measures the
// candidate replacements on the SAME committed corpora the floor lane gates
// (`aterm_bench::{rotating,replog,linkheavy}_corpus` — the third is the
// Wave-4A P7 hyperlink-heavy shape), through the REAL terminal fill, so the
// per-trigram posting distributions are the production ones, not synthetic.
//
// Candidates:
//   btreeset   HashMap<trigram, BTreeSet<u32>>   — the shipping shape.
//   sortedvec  HashMap<trigram, Vec<u32>>        — ascending array; terminal
//              appends arrive in row order, so inserts are push-back.
//   runlength  HashMap<trigram, Vec<(u32,u32)>>  — closed intervals; the
//              roaring "run container" representative. A full roaring hybrid
//              degenerates to array or run containers at terminal posting
//              densities, so sortedvec/runlength bound it from both sides.
//
// Per corpus x container: net retained heap of the posting map (counting
// allocator, B/line over grid rows), container build klines/s from one shared
// neutral posting set (isolates container cost from text/parse cost), and
// intersect+count throughput for the lane's multi-trigram query.
//
//   cargo run --release -q -p aterm-bench --example posting_container_harness
//
// Deterministic (no RNG/clock); median-of-N; informational JSON (a DECISION
// bench — floors arrive with the adopted winner, not before).

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeSet, HashMap};
use std::hint::black_box;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use aterm_bench::{linkheavy_corpus, replog_corpus, rotating_corpus};
use aterm_core::terminal::TerminalBuilder;

static NET: AtomicI64 = AtomicI64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn net() -> i64 {
    NET.load(Ordering::Relaxed)
}

const ROWS: u16 = 24;
const COLS: u16 = 80;
const RING_LINES: usize = 60_000;
const CORPUS_LINES: usize = 50_000;
const LINKHEAVY_LINES: usize = 25_000;
const N_ITERS: usize = 5;
const WARMUP: usize = 1;
const INTERSECT_BATCH: usize = 50;

/// Extract every retained row's text through the real terminal — the exact
/// line set `Terminal::build_search_index` indexes (history then visible).
fn corpus_rows(corpus: &[u8]) -> Vec<String> {
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(RING_LINES)
        .build();
    term.process(black_box(corpus));
    let grid = term.grid();
    let scrollback = grid.scrollback_lines();
    let mut rows: Vec<String> = (0..scrollback)
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string())
                .unwrap_or_default()
        })
        .collect();
    for r in 0..term.rows() {
        rows.push(term.get_line_text(i32::from(r), None).unwrap_or_default());
    }
    rows
}

/// Neutral per-trigram posting lists (ascending row ids, deduplicated),
/// mirroring `SearchIndex::index_line`'s insert pattern: original-case
/// trigrams plus a lowered pass when lowering changes bytes. The corpora are
/// ASCII, so `to_ascii_lowercase` equals the engine's Unicode fold here.
fn neutral_postings(rows: &[String]) -> Vec<([u8; 3], Vec<u32>)> {
    let mut map: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
    let mut add = |bytes: &[u8], row: u32| {
        for w in bytes.windows(3) {
            let t = [w[0], w[1], w[2]];
            let list = map.entry(t).or_default();
            if list.last() != Some(&row) {
                list.push(row);
            }
        }
    };
    for (i, text) in rows.iter().enumerate() {
        let row = u32::try_from(i).expect("bench rows fit u32");
        add(text.as_bytes(), row);
        let needs_lower = !text.is_ascii() || text.bytes().any(|b| b.is_ascii_uppercase());
        if needs_lower {
            add(text.to_ascii_lowercase().as_bytes(), row);
        }
    }
    let mut out: Vec<_> = map.into_iter().collect();
    out.sort_unstable_by_key(|(t, _)| *t);
    out
}

/// One candidate container built from the neutral posting set.
enum Container {
    BTree(HashMap<[u8; 3], BTreeSet<u32>>),
    SortedVec(HashMap<[u8; 3], Vec<u32>>),
    RunLength(HashMap<[u8; 3], Vec<(u32, u32)>>),
}

impl Container {
    fn build(kind: &str, postings: &[([u8; 3], Vec<u32>)]) -> Self {
        match kind {
            "btreeset" => {
                let mut map: HashMap<[u8; 3], BTreeSet<u32>> = HashMap::new();
                for (t, rows) in postings {
                    let set = map.entry(*t).or_default();
                    for &r in rows {
                        set.insert(r);
                    }
                }
                Container::BTree(map)
            }
            "sortedvec" => {
                let mut map: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
                for (t, rows) in postings {
                    // Push-back inserts: rows arrive ascending, as on a live
                    // terminal append stream.
                    let list = map.entry(*t).or_default();
                    for &r in rows {
                        list.push(r);
                    }
                }
                Container::SortedVec(map)
            }
            "runlength" => {
                let mut map: HashMap<[u8; 3], Vec<(u32, u32)>> = HashMap::new();
                for (t, rows) in postings {
                    let runs = map.entry(*t).or_default();
                    for &r in rows {
                        match runs.last_mut() {
                            Some((_, hi)) if *hi + 1 == r => *hi = r,
                            Some((_, hi)) if *hi >= r => {}
                            _ => runs.push((r, r)),
                        }
                    }
                }
                Container::RunLength(map)
            }
            _ => unreachable!("unknown container kind"),
        }
    }

    /// Intersect the posting lists for every trigram of `query`, returning
    /// the number of candidate rows (what the verification stage consumes).
    fn intersect_count(&self, query: &[u8]) -> usize {
        let trigrams: Vec<[u8; 3]> = query.windows(3).map(|w| [w[0], w[1], w[2]]).collect();
        match self {
            Container::BTree(map) => {
                let mut lists: Vec<&BTreeSet<u32>> = Vec::new();
                for t in &trigrams {
                    let Some(s) = map.get(t) else { return 0 };
                    lists.push(s);
                }
                lists.sort_unstable_by_key(|s| s.len());
                let mut acc: BTreeSet<u32> = lists[0].clone();
                for s in &lists[1..] {
                    acc = acc.intersection(s).copied().collect();
                }
                acc.len()
            }
            Container::SortedVec(map) => {
                let mut lists: Vec<&Vec<u32>> = Vec::new();
                for t in &trigrams {
                    let Some(v) = map.get(t) else { return 0 };
                    lists.push(v);
                }
                lists.sort_unstable_by_key(|v| v.len());
                let mut acc: Vec<u32> = lists[0].clone();
                for v in &lists[1..] {
                    let mut out = Vec::with_capacity(acc.len());
                    let (mut i, mut j) = (0usize, 0usize);
                    while i < acc.len() && j < v.len() {
                        match acc[i].cmp(&v[j]) {
                            std::cmp::Ordering::Less => i += 1,
                            std::cmp::Ordering::Greater => j += 1,
                            std::cmp::Ordering::Equal => {
                                out.push(acc[i]);
                                i += 1;
                                j += 1;
                            }
                        }
                    }
                    acc = out;
                }
                acc.len()
            }
            Container::RunLength(map) => {
                let mut lists: Vec<&Vec<(u32, u32)>> = Vec::new();
                for t in &trigrams {
                    let Some(v) = map.get(t) else { return 0 };
                    lists.push(v);
                }
                lists.sort_unstable_by_key(|v| v.len());
                let mut acc: Vec<(u32, u32)> = lists[0].clone();
                for v in &lists[1..] {
                    let mut out = Vec::with_capacity(acc.len());
                    let (mut i, mut j) = (0usize, 0usize);
                    while i < acc.len() && j < v.len() {
                        let (alo, ahi) = acc[i];
                        let (blo, bhi) = v[j];
                        let lo = alo.max(blo);
                        let hi = ahi.min(bhi);
                        if lo <= hi {
                            out.push((lo, hi));
                        }
                        if ahi <= bhi {
                            i += 1;
                        } else {
                            j += 1;
                        }
                    }
                    acc = out;
                }
                acc.iter()
                    .map(|(lo, hi)| (hi - lo) as usize + 1)
                    .sum::<usize>()
            }
        }
    }
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

struct ContainerReport {
    bytes_per_line: f64,
    build_klps: f64,
    intersect_qps: f64,
    candidates: usize,
}

fn measure_container(
    kind: &str,
    postings: &[([u8; 3], Vec<u32>)],
    row_count: usize,
    query: &[u8],
) -> ContainerReport {
    // Retained memory: net heap across one build, container map only.
    let before = net();
    let built = Container::build(kind, postings);
    let bytes = (net() - before).max(0) as f64;
    let bytes_per_line = bytes / row_count.max(1) as f64;

    // Build cost from the neutral set (container insert cost in klines/s).
    let mut build = Vec::with_capacity(N_ITERS);
    let pass = || -> f64 {
        let t0 = Instant::now();
        let c = Container::build(kind, postings);
        let secs = t0.elapsed().as_secs_f64();
        black_box(&c);
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        (row_count as f64 / 1000.0) / secs
    };
    for _ in 0..WARMUP {
        let _ = pass();
    }
    for _ in 0..N_ITERS {
        build.push(pass());
    }

    // Intersect+count throughput for the lane query's trigrams.
    let mut candidates = 0usize;
    let mut inter = Vec::with_capacity(N_ITERS);
    let mut ipass = || -> f64 {
        let t0 = Instant::now();
        for _ in 0..INTERSECT_BATCH {
            candidates = built.intersect_count(black_box(query));
            black_box(candidates);
        }
        let secs = t0.elapsed().as_secs_f64();
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        INTERSECT_BATCH as f64 / secs
    };
    for _ in 0..WARMUP {
        let _ = ipass();
    }
    for _ in 0..N_ITERS {
        inter.push(ipass());
    }

    ContainerReport {
        bytes_per_line,
        build_klps: median(&build),
        intersect_qps: median(&inter),
        candidates,
    }
}

fn main() {
    let corpora: [(&str, Vec<u8>, &[u8]); 3] = [
        ("rotating", rotating_corpus(CORPUS_LINES), b"jklmno"),
        ("replog", replog_corpus(CORPUS_LINES), b"svc-worker"),
        ("linkheavy", linkheavy_corpus(LINKHEAVY_LINES), b"registry"),
    ];
    let kinds = ["btreeset", "sortedvec", "runlength"];

    let mut json = String::from("{");
    for (name, corpus, query) in &corpora {
        let rows = corpus_rows(corpus);
        let postings = neutral_postings(&rows);
        let entries: usize = postings.iter().map(|(_, v)| v.len()).sum();
        eprintln!(
            "posting_container_harness: {name} — {} rows, {} trigrams, {} posting entries",
            rows.len(),
            postings.len(),
            entries,
        );
        for kind in kinds {
            let rep = measure_container(kind, &postings, rows.len(), query);
            eprintln!(
                "  {kind:9} — {:7.1} B/line | build {:8.1} klines/s | intersect {:9.0} q/s \
                 ({} candidates)",
                rep.bytes_per_line, rep.build_klps, rep.intersect_qps, rep.candidates,
            );
            json.push_str(&format!(
                "\"{name}_{kind}_bytes_per_line\":{:.1},\
                 \"{name}_{kind}_build_klps\":{:.3},\
                 \"{name}_{kind}_intersect_qps\":{:.3},",
                rep.bytes_per_line, rep.build_klps, rep.intersect_qps,
            ));
        }
    }
    json.push_str(&format!(
        "\"corpus_lines\":{CORPUS_LINES},\"linkheavy_lines\":{LINKHEAVY_LINES},\
         \"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    ));
    println!("{json}");
}
