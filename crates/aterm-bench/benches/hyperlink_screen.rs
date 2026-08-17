// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Hyperlink / extras-heavy engine throughput (ROADMAP WS-K, ATERM_DESIGN §7).
//
// WHY THIS EXISTS: `CellExtras::enforce_hyperlink_limit` sits on two hot caller
// chains — the write path calls it once per written RUN that carries an OSC 8
// link (`write_ascii_run_with_extras` -> `set_range_uniform`; the per-character
// fallback `apply_cell_extras_preflagged` calls it per cell), and every
// scroll/region shift in `extra_collection_shifts.rs` calls it after re-keying
// the map. Yet NOT ONE corpus in `engine_throughput` puts a single entry in the
// extras map — its terminals end with `extras().len() == 0` — so that whole limb
// of the engine was unmeasured, and any change to it could only be argued from a
// caller chain, never observed. That is what this file fixes.
//
// The three workloads land in three DIFFERENT states of that function, because
// they cost different things:
//
//   hyperlink_dense           `ls --hyperlink` / `eza`: a saturated 24x80 screen
//                             of DISTINCT URIs that scrolls on every row. The
//                             cost here is the O(1) `row_offset` amortization
//                             and its compaction: scrolled-off entries stay in
//                             the map until compacted, so the map crosses the
//                             limit on STALE entries alone (measured: ~28 times
//                             per MiB) even though only 1_680 are ever live.
//                             NOTE it does NOT reach `extra_collection_shifts`:
//                             this corpus emits only text and CR/LF, never
//                             DECSTBM/IL/DL/ICH/DCH, so `shift_region_up_by` and
//                             its siblings are not on this path. Plain LF
//                             scrolling takes the row_offset lane instead.
//
//   hyperlink_over_limit      More live hyperlink-bearing cells than
//                             MAX_HYPERLINK_ENTRIES (20_000 on a 100x200 grid,
//                             2x the limit), so the cold path genuinely EVICTS:
//                             collect, `select_nth_unstable_by`, clear, shrink,
//                             down to the 75 % low-water mark. Measured: 84
//                             evictions per MiB, each shedding ~2_300 entries.
//
//   mixed_extras_under_limit  THE CASE THE COLD PATH IS WRONG FOR. The map is
//                             over MAX_HYPERLINK_ENTRIES but the HYPERLINK count
//                             is far under it — a big colourful screen (SGR 58
//                             coloured underlines) with only a few hyperlinked
//                             lines on it. `enforce_hyperlink_limit` gates on the
//                             TOTAL map size, so every hyperlinked run walks all
//                             14_600 entries and allocates a coord Vec, then
//                             discovers 600 <= 10_000 and evicts NOTHING.
//                             Measured: 1_835 such walks per MiB, zero evictions.
//
// `verify_reaches_target` asserts each workload really is in its intended state
// before it is timed. A benchmark that misses the code under test is worse than
// no benchmark, and these states are easy to lose to an innocent edit of a
// corpus shape (one row fewer and the mixed map drops under the limit, taking
// the O(1) early-out and measuring nothing).

use std::fmt::Write as _;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const CORPUS_BYTES: usize = 1 << 20; // 1 MiB per workload, matching engine_throughput

/// A realistic `ls --hyperlink` screen — the same 24x80 engine_throughput uses.
const DENSE_ROWS: u16 = 24;
const DENSE_COLS: u16 = 80;

/// The two over-limit workloads need more than `MAX_HYPERLINK_ENTRIES` (10_000)
/// extras entries resident AT ONCE, and extras are keyed by live grid
/// coordinate — so the grid itself has to be big enough to hold them. 100x200 =
/// 20_000 cells, 2x the limit, which is what lets these two reach (and stay in)
/// their target states from live cells rather than from scrolled-off leftovers.
/// It is also a plausible real geometry: a maximized window on a 4K display.
const WIDE_ROWS: u16 = 100;
const WIDE_COLS: u16 = 200;

/// Rows of the wide grid filled with NON-hyperlink extras in the mixed workload.
/// 70 * 200 = 14_000 entries — past the 10_000 limit on their own, with no
/// hyperlink among them.
const MIXED_BULK_ROWS: u16 = 70;
/// Rows of the wide grid that DO carry hyperlinks in the mixed workload.
/// 3 * 200 = 600 hyperlinks — 6 % of the limit, which is the whole point: the
/// map is over budget, the hyperlinks are nowhere near it.
const MIXED_LINK_ROWS: u16 = 3;

/// Ten 20-column links fill one 200-column row of the wide grid exactly.
const WIDE_LINKS_PER_ROW: usize = 10;

/// Granularity of the state sampling in `verify_reaches_target`. Small enough
/// that a single eviction cannot be hidden by the writes that follow it before
/// the next sample.
const SAMPLE_CHUNK: usize = 1024;

/// OSC 8 close: `OSC 8 ; <empty params> ; <empty URI> ST` — see `handler_osc_8`,
/// which reads params[1] as the params field and params[2..] as the URI, and
/// treats an empty URI as "end hyperlink".
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// Append `OSC 8 ; ; URI ST` opening a link to a URI unique to `n`.
///
/// DISTINCT URIs matter: a single shared URI would still create one map entry
/// per cell, but every entry would clone one `Arc<str>`, so neither the
/// allocation traffic nor the `Arc` refcount pattern of a real `ls --hyperlink`
/// would be reproduced. `https` is on `is_allowed_scheme`'s allowlist, so these
/// links are actually accepted instead of being silently dropped — a rejected
/// scheme would leave the extras map empty and quietly measure nothing.
fn push_link_open(out: &mut String, n: u32, ext: &str) {
    let _ = write!(out, "\x1b]8;;https://f.example/p/{n:06}{ext}\x1b\\");
}

/// Append one 20-column hyperlinked run: `link<n>.dat______`, all 20 columns
/// inside the link. Ten of these fill a 200-column row.
fn push_wide_link_cell(out: &mut String, n: u32) {
    push_link_open(out, n, ".dat");
    let _ = write!(out, "link{n:06}.dat______"); // 4 + 6 + 4 + 6 = 20 columns
    out.push_str(LINK_CLOSE);
}

/// What a workload has to be verified to have reached before it is timed.
enum Target {
    /// Live screen saturated with distinct hyperlinks, scrolling on every row.
    SaturatedAndScrolling,
    /// Hyperlink count past the limit, so the cold path really evicts.
    Evicting,
    /// Map past the limit, hyperlinks far under it, nothing ever evicted.
    ColdPathNoEviction,
}

struct Workload {
    name: &'static str,
    rows: u16,
    cols: u16,
    target: Target,
    corpus: Vec<u8>,
}

/// `ls --hyperlink` / `eza`: five 16-column entries per 80-column row, each a
/// 14-character filename wrapped in its own OSC 8 link (so 87.5 % of cells carry
/// a hyperlink; the two-space gutter is outside the link, as in real `ls`),
/// terminated by `\r\n`. Every row past the 24th scrolls the whole screen, which
/// is what puts this workload on the shift-path callers.
fn corpus_hyperlink_dense() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 512);
    let mut n: u32 = 0;
    while out.len() < CORPUS_BYTES {
        for _ in 0..5 {
            push_link_open(&mut out, n, ".txt");
            let _ = write!(out, "name{n:06}.txt");
            out.push_str(LINK_CLOSE);
            out.push_str("  "); // gutter between columns, outside the link
            n = n.wrapping_add(1);
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// Saturate the whole 100x200 grid with distinct hyperlinks — 20_000
/// hyperlink-bearing cells, 2x `MAX_HYPERLINK_ENTRIES`.
///
/// Rows are addressed with CUP so the screen is repainted IN PLACE and never
/// scrolls: with no scroll there is no `row_offset` and no compaction, so the
/// entries the cold path counts are all live and the eviction it performs is
/// unambiguously about the hyperlink budget rather than about stale leftovers.
fn corpus_hyperlink_over_limit() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 4096);
    let mut n: u32 = 0;
    // One whole screen per outer step, so the corpus always ends on a complete
    // repaint and the terminal state after a run is deterministic.
    while out.len() < CORPUS_BYTES {
        for row in 1..=WIDE_ROWS {
            let _ = write!(out, "\x1b[{row};1H");
            for _ in 0..WIDE_LINKS_PER_ROW {
                push_wide_link_cell(&mut out, n);
                n = n.wrapping_add(1);
            }
        }
    }
    out.into_bytes()
}

/// A big colourful screen with a few hyperlinked lines on it: 70 rows of SGR 58
/// coloured-underline cells (14_000 non-hyperlink extras entries) plus 3 rows of
/// hyperlinks (600 entries).
///
/// SGR 58 is the cheapest ordinary way to get non-hyperlink extras in bulk — it
/// sets `transient.current_underline_color`, which makes `needs_style_extras`
/// true for every printed cell, so each one takes `cell_extra_mut_preflagged`
/// and lands a real entry in the map. Combining marks would work too but cost
/// grapheme work that has nothing to do with what is being measured.
///
/// Also CUP-positioned and non-scrolling, so the map settles at 14_600 entries /
/// 600 hyperlinks on the first repaint and stays there for the whole run: every
/// subsequent hyperlinked run walks 14_600 entries, allocates, and evicts nothing.
fn corpus_mixed_extras_under_limit() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 4096);
    let mut n: u32 = 0;
    while out.len() < CORPUS_BYTES {
        for row in 1..=MIXED_BULK_ROWS {
            // Vary the colour per row; a real TUI is not monochrome, and a
            // single hoisted SGR would not re-exercise the SGR 58 parse.
            let color = 17 + u32::from(row) % 215;
            let _ = write!(out, "\x1b[{row};1H\x1b[58;5;{color}m");
            for _ in 0..WIDE_COLS {
                out.push('#');
            }
        }
        // Drop the underline colour so the hyperlink rows carry ONLY a
        // hyperlink — an entry whose sole datum is the link is the one the
        // eviction path would delete outright, which keeps this workload's
        // "evicts nothing" claim about the count and not about `has_data`.
        out.push_str("\x1b[0m");
        for row in (MIXED_BULK_ROWS + 1)..=(MIXED_BULK_ROWS + MIXED_LINK_ROWS) {
            let _ = write!(out, "\x1b[{row};1H");
            for _ in 0..WIDE_LINKS_PER_ROW {
                push_wide_link_cell(&mut out, n);
                n = n.wrapping_add(1);
            }
        }
    }
    out.into_bytes()
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "hyperlink_dense",
            rows: DENSE_ROWS,
            cols: DENSE_COLS,
            target: Target::SaturatedAndScrolling,
            corpus: corpus_hyperlink_dense(),
        },
        Workload {
            name: "hyperlink_over_limit",
            rows: WIDE_ROWS,
            cols: WIDE_COLS,
            target: Target::Evicting,
            corpus: corpus_hyperlink_over_limit(),
        },
        Workload {
            name: "mixed_extras_under_limit",
            rows: WIDE_ROWS,
            cols: WIDE_COLS,
            target: Target::ColdPathNoEviction,
            corpus: corpus_mixed_extras_under_limit(),
        },
    ]
}

/// (live extras entries, of which hyperlink-bearing).
///
/// `CellExtras::len`/`iter` report LIVE entries — with `row_offset == 0` (the
/// two non-scrolling workloads) that is exactly the map size the cold path's
/// O(1) guard tests.
fn extras_state(term: &aterm_core::terminal::Terminal) -> (usize, usize) {
    let extras = term.grid().extras();
    let hyperlinks = extras
        .iter()
        .filter(|(_, extra)| extra.hyperlink().is_some())
        .count();
    (extras.len(), hyperlinks)
}

/// Prove the workload reaches the code it claims to, before it is timed.
///
/// Feeds the corpus in small chunks and watches the extras map. The corpora only
/// ever ADD entries (they repaint the same coordinates or scroll forward), so a
/// DECREASE in the entry count can come from exactly one place: the low-water
/// trim inside `enforce_hyperlink_limit_cold`, which only runs once the map is
/// past `MAX_HYPERLINK_ENTRIES`. That makes `biggest_drop` a direct, external
/// witness that the cold path ran and evicted — and `biggest_drop == 0` next to
/// an over-limit map an equally direct witness that it ran and did not.
fn verify_reaches_target(w: &Workload) {
    let limit = aterm_grid::extra_collection::CellExtras::max_hyperlink_entries();
    let mut term = aterm_core::terminal::Terminal::new(w.rows, w.cols);
    let mut peak_entries = 0usize;
    let mut peak_links = 0usize;
    let mut biggest_drop = 0usize;
    let mut prev = 0usize;
    for chunk in w.corpus.chunks(SAMPLE_CHUNK) {
        term.process(chunk);
        let (entries, links) = extras_state(&term);
        peak_entries = peak_entries.max(entries);
        peak_links = peak_links.max(links);
        biggest_drop = biggest_drop.max(prev.saturating_sub(entries));
        prev = entries;
    }
    let (entries, links) = extras_state(&term);

    match w.target {
        Target::SaturatedAndScrolling => {
            // HONEST SCOPE OF THIS GUARD: it pins the workload's INPUT state (a
            // hyperlink-saturated, scrolling screen) — it does NOT prove the
            // cold path is entered, and cannot from out here. Cold-path entry
            // for this workload happens on STALE scrolled-off entries, and
            // `extras().len()` filters exactly those, so the crossing is
            // invisible to any public accessor. It was established once, out of
            // band, with a temporary counting allocator keyed on `CellCoord`'s
            // distinctive 2-byte alignment: ~28 cold-path entries per MiB, each
            // collecting >10_000 coords. If you need that number again,
            // re-instrument; do not read it off these assertions.
            let saturated = usize::from(DENSE_ROWS) * 60;
            assert!(
                peak_entries >= saturated,
                "{}: expected a hyperlink-saturated screen (>= {saturated} live \
                 entries), got {peak_entries} — is OSC 8 still being accepted?",
                w.name
            );
            assert_eq!(
                links, entries,
                "{}: every entry on this screen should be hyperlink-bearing",
                w.name
            );
        }
        Target::Evicting => {
            assert!(
                peak_links > limit * 4 / 5,
                "{}: hyperlink count peaked at {peak_links}, nowhere near the \
                 {limit} limit — this workload is meant to blow through it",
                w.name
            );
            assert!(
                biggest_drop >= 1_000,
                "{}: no eviction observed (biggest entry-count drop {biggest_drop}); \
                 the corpus only ever adds entries, so the cold path never trimmed",
                w.name
            );
        }
        Target::ColdPathNoEviction => {
            assert!(
                entries > limit,
                "{}: extras map is {entries} entries, at or under the {limit} \
                 limit — `enforce_hyperlink_limit` would take its O(1) early-out \
                 and this workload would measure nothing",
                w.name
            );
            // BOTH bounds, not just the upper one. Asserting only
            // `peak_links * 4 < limit` is satisfied by `peak_links == 0` — the
            // 14_000 coloured-underline entries alone push `entries` past the
            // limit — and a corpus with no hyperlinks at all would never call
            // `enforce_hyperlink_limit` in the first place, so the workload
            // would measure nothing while every assertion here passed.
            assert!(
                peak_links > 0,
                "{}: NO hyperlink-bearing entries — nothing calls \
                 `enforce_hyperlink_limit` at all, so this workload measures \
                 nothing. Is OSC 8 still being accepted?",
                w.name
            );
            assert!(
                peak_links * 4 < limit,
                "{}: hyperlink count peaked at {peak_links}, too close to the \
                 {limit} limit — this workload must stay COMFORTABLY under it",
                w.name
            );
            assert_eq!(
                biggest_drop, 0,
                "{}: something evicted; this workload is the one where the cold \
                 path walks the whole map and then evicts nothing",
                w.name
            );
        }
    }
}

fn hyperlink_screen(c: &mut Criterion) {
    let mut group = c.benchmark_group("hyperlink_screen");
    for w in workloads() {
        verify_reaches_target(&w);
        group.throughput(Throughput::Bytes(w.corpus.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w, |b, w| {
            b.iter(|| {
                // Fresh engine per iteration, as engine_throughput does: the
                // extras map is rebuilt from empty every time, so one iteration
                // is one honest "shell dumps a hyperlinked screen at you".
                let mut term = aterm_core::terminal::Terminal::new(w.rows, w.cols);
                term.process(black_box(&w.corpus));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, hyperlink_screen);
criterion_main!(benches);
