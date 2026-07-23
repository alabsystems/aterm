// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Allocation/time micro-bench for the `extract` vs `extract_into` snapshot path.
//
// `Renderer::extract` builds a fresh `RenderInput` every frame: the four per-row
// container Vecs (`cells`, `clusters`, `combining`, `line_sizes`) are allocated
// anew each call via `.collect()`. `Renderer::extract_into` instead REFILLS a
// caller-owned scratch in place — `.clear()` keeps each container's heap
// capacity, so a stable-dimension frame stops allocating those four containers.
//
// `bench_extract_perrow_reuse` isolates the FURTHER win from per-row buffer
// reuse: it compares the OLD `extract_into` container strategy (clear the outer
// Vec-of-Vecs, which DROPS every inner per-row Vec, then `.collect()` a fresh
// `Vec<RenderCell>`/cluster/combining Vec per row from the `*_row` accessors)
// against the NEW one (`resize_with` the outer Vec keeping the inner per-row
// Vecs, then `*_row_into` clears + refills each in place). Both run on a warm,
// dimension-stable kept scratch — the realistic steady state. The OLD path is
// replicated INLINE here (`extract_into_old`) so this one binary measures both;
// the production `extract_into` IS the new path.
//
// This is its OWN test binary so the process-global counting allocator below
// only instruments these benches, never the correctness tests in the sibling
// files. All benches are `#[ignore]`d (they print numbers, they don't assert a
// machine-dependent threshold); run with:
//   cargo test -p aterm-render --test extract_reuse_bench -- --ignored --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use aterm_core::grid::LineSize;
use aterm_core::terminal::Terminal;
use aterm_render::RenderInput;

/// A pass-through allocator that COUNTS allocation calls while `ACTIVE` is set.
/// Counting is gated so warm-up / setup allocations don't pollute the figure;
/// flip `ACTIVE` around just the region under test.
struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: forwarding the caller's already-valid `layout` to the System
        // allocator; this type is a pure pass-through that only counts.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` came from `System.alloc` via this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn counted<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    let r = f();
    ACTIVE.store(false, Ordering::Relaxed);
    (
        r,
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

/// A representative full screen: every row filled with text + a cursor move, so
/// `render_row`/`cluster_row`/`combining_row` each produce real per-row data.
fn filled_term(rows: usize, cols: usize) -> Terminal {
    let mut term = Terminal::new(rows as u16, cols as u16);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H", r + 1).as_bytes());
        term.process(b"the quick brown fox jumps over the lazy dog 0123456789");
    }
    term
}

/// The OLD `extract_into` container strategy, replicated here for a before/after
/// comparison against the production (new) `Renderer::extract_into`. It REFILLS
/// the caller's `scratch` but `.clear()`s each outer Vec-of-Vecs — which DROPS
/// every inner per-row Vec — then `.collect()`s a fresh inner Vec per row from
/// the `*_row` accessors. So even on a warm, dimension-stable scratch the outer
/// containers are reused but EVERY per-row Vec is reallocated each frame.
/// (`line_sizes` was already `.clear()` + extend in both versions; identical.)
fn extract_into_old(scratch: &mut RenderInput, term: &Terminal, rows: usize, cols: usize) {
    scratch.rows = rows;
    scratch.cols = cols;

    scratch.cells.clear();
    scratch.cells.extend((0..rows).map(|r| term.render_row(r)));

    scratch.clusters.clear();
    scratch
        .clusters
        .extend((0..rows).map(|r| term.cluster_row(r)));

    scratch.combining.clear();
    scratch
        .combining
        .extend((0..rows).map(|r| term.combining_row(r)));

    scratch.line_sizes.clear();
    scratch.line_sizes.extend((0..rows).map(|r| {
        u16::try_from(r)
            .ok()
            .and_then(|vr| term.grid().row(vr))
            .map_or(LineSize::SingleWidth, |row| row.line_size())
    }));

    let cur = term.cursor();
    scratch.cursor_row = cur.row as usize;
    scratch.cursor_col = cur.col as usize;
    scratch.cursor_visible = term.cursor_visible();
    scratch.cursor_style = term.cursor_style();
    scratch.display_offset = term.grid().display_offset() as i32;
    scratch.selection.clone_from(term.text_selection());
}

/// BEFORE vs AFTER the per-row buffer reuse. Both run on a warm, dimension-stable
/// kept scratch (the realistic steady state). `old` = clear-outer + collect-fresh
/// inner Vecs every frame; `new` = `Renderer::extract_into` = resize-in-place +
/// `*_row_into` clear/refill. Both produce a byte-identical `RenderInput`
/// (asserted here), so the only difference measured is allocation/time.
#[test]
#[ignore]
fn bench_extract_perrow_reuse() {
    let (rows, cols) = (40usize, 120usize);
    let mut term = filled_term(rows, cols);
    let iters = 500u64;

    // Warm each scratch outside the counted region so outer-container capacity
    // is already grown — isolating the PER-ROW inner-Vec allocations. A few
    // warm iterations let the System allocator settle into steady state so the
    // count reflects the production hot loop, not first-touch growth or heap
    // fragmentation left over from another bench in the same test process.
    let mut new_scratch = RenderInput::default();
    let mut old_scratch = RenderInput::default();
    for _ in 0..64 {
        term.cell_frame_into(&mut new_scratch, rows, cols);
        extract_into_old(&mut old_scratch, &term, rows, cols);
    }

    // Count the REUSE (new) path first: it must be measured before the churning
    // `old` loop frees 40 Vecs/frame and fragments the heap under it.
    let (_, new_allocs, new_bytes) = counted(|| {
        for _ in 0..iters {
            term.cell_frame_into(&mut new_scratch, rows, cols);
            std::hint::black_box(&new_scratch);
        }
    });
    let (_, old_allocs, old_bytes) = counted(|| {
        for _ in 0..iters {
            extract_into_old(&mut old_scratch, &term, rows, cols);
            std::hint::black_box(&old_scratch);
        }
    });

    // Same data — the optimization is allocation-only, not semantic.
    assert_eq!(
        old_scratch, new_scratch,
        "old and new extract paths must be byte-identical"
    );

    // Timing, separate loops (no allocator counting).
    let t_old = {
        let s = Instant::now();
        for _ in 0..iters {
            extract_into_old(&mut old_scratch, &term, rows, cols);
            std::hint::black_box(&old_scratch);
        }
        s.elapsed()
    };
    let t_new = {
        let s = Instant::now();
        for _ in 0..iters {
            term.cell_frame_into(&mut new_scratch, rows, cols);
            std::hint::black_box(&new_scratch);
        }
        s.elapsed()
    };

    let per_old = old_allocs as f64 / iters as f64;
    let per_new = new_allocs as f64 / iters as f64;
    eprintln!("--- per-row buffer reuse: BEFORE vs AFTER ({rows}x{cols} ASCII, warm scratch) ---");
    eprintln!(
        "  allocs/frame:  before={per_old:.1}  after={per_new:.1}  \
         (saved {:.1}/frame, {:.1}%)",
        per_old - per_new,
        if per_old > 0.0 {
            (per_old - per_new) / per_old * 100.0
        } else {
            0.0
        }
    );
    eprintln!(
        "  bytes/frame:   before={:.0}  after={:.0}  (saved {:.0}/frame)",
        old_bytes as f64 / iters as f64,
        new_bytes as f64 / iters as f64,
        (old_bytes.saturating_sub(new_bytes)) as f64 / iters as f64,
    );
    eprintln!(
        "  time/frame:    before={:.2} us  after={:.2} us  (saved {:.2} us/frame, {:.1}%)",
        t_old.as_secs_f64() / iters as f64 * 1e6,
        t_new.as_secs_f64() / iters as f64 * 1e6,
        (t_old.as_secs_f64() - t_new.as_secs_f64()) / iters as f64 * 1e6,
        if t_old.as_secs_f64() > 0.0 {
            (t_old.as_secs_f64() - t_new.as_secs_f64()) / t_old.as_secs_f64() * 100.0
        } else {
            0.0
        },
    );

    assert!(
        new_allocs < old_allocs,
        "per-row reuse must allocate fewer times: after={new_allocs} before={old_allocs}"
    );
}

#[test]
#[ignore]
fn bench_extract_alloc_count() {
    let (rows, cols) = (40usize, 120usize);
    let mut term = filled_term(rows, cols);
    let iters = 500u64;

    // REUSE: one scratch, refilled in place each frame. Warm it for several
    // iterations outside the counted region so its container AND inner per-row
    // capacities are already grown (the realistic steady state: same window,
    // frame after frame). Measure this BEFORE the fresh path below churns the
    // heap (it alloc/frees a whole `RenderInput` per frame), which would
    // otherwise fragment the allocator under the warm scratch.
    let mut scratch = RenderInput::default();
    for _ in 0..64 {
        term.cell_frame_into(&mut scratch, rows, cols);
    }
    let (_, reuse_allocs, reuse_bytes) = counted(|| {
        for _ in 0..iters {
            term.cell_frame_into(&mut scratch, rows, cols);
            std::hint::black_box(&scratch);
        }
    });

    // FRESH-EACH-FRAME: the pre-reuse path. One `RenderInput` allocated per
    // frame (outer + per-row Vecs), dropped at end of iter.
    let (_, fresh_allocs, fresh_bytes) = counted(|| {
        for _ in 0..iters {
            let input = term.cell_frame(rows, cols);
            std::hint::black_box(&input);
        }
    });

    let per_fresh = fresh_allocs as f64 / iters as f64;
    let per_reuse = reuse_allocs as f64 / iters as f64;
    eprintln!(
        "extract allocs/frame:        fresh={per_fresh:.1}  reuse={per_reuse:.1}  \
         (saved {:.1}/frame, {:.1}%)",
        per_fresh - per_reuse,
        if per_fresh > 0.0 {
            (per_fresh - per_reuse) / per_fresh * 100.0
        } else {
            0.0
        }
    );
    eprintln!(
        "extract bytes/frame:         fresh={:.0}  reuse={:.0}  (saved {:.0}/frame)",
        fresh_bytes as f64 / iters as f64,
        reuse_bytes as f64 / iters as f64,
        (fresh_bytes - reuse_bytes) as f64 / iters as f64,
    );
    eprintln!("  ({rows}x{cols} grid, {iters} frames, warm scratch)");

    // The reuse path MUST allocate strictly fewer times per frame than fresh —
    // it elides the four container Vecs. (It still allocates the inner per-row
    // Vecs/Boxes returned by the aterm-core accessors, so it isn't zero.)
    assert!(
        reuse_allocs < fresh_allocs,
        "reuse should allocate fewer times: reuse={reuse_allocs} fresh={fresh_allocs}"
    );
}

#[test]
#[ignore]
fn bench_extract_time() {
    let (rows, cols) = (40usize, 120usize);
    let mut term = filled_term(rows, cols);
    let iters = 5000u64;

    let t_fresh = {
        let start = Instant::now();
        for _ in 0..iters {
            let input = term.cell_frame(rows, cols);
            std::hint::black_box(&input);
        }
        start.elapsed()
    };

    let mut scratch = RenderInput::default();
    for _ in 0..64 {
        term.cell_frame_into(&mut scratch, rows, cols);
    }
    let t_reuse = {
        let start = Instant::now();
        for _ in 0..iters {
            term.cell_frame_into(&mut scratch, rows, cols);
            std::hint::black_box(&scratch);
        }
        start.elapsed()
    };

    let per_fresh = t_fresh.as_secs_f64() / iters as f64 * 1e6;
    let per_reuse = t_reuse.as_secs_f64() / iters as f64 * 1e6;
    eprintln!(
        "extract time/frame:          fresh={per_fresh:.2} us  reuse={per_reuse:.2} us  \
         (saved {:.2} us/frame, {:.1}%)",
        per_fresh - per_reuse,
        if per_fresh > 0.0 {
            (per_fresh - per_reuse) / per_fresh * 100.0
        } else {
            0.0
        }
    );
    eprintln!("  ({rows}x{cols} grid, {iters} frames, warm scratch)");
}
