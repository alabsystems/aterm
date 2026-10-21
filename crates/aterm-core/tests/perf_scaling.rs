// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Engine complexity guard that counts heap allocation calls, not wall time, so it is
//! deterministic and machine-independent. The engine writes into a fixed grid with a capped
//! scrollback, so per-byte or per-line allocation shows up as growth. The retained-heap
//! ceiling is `mem_budget.rs`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);

/// System allocator that counts allocation calls while `ACTIVE` (only the measured spans).
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// `lines` rows of ASCII + a little SGR, each ending in `\r\n` so the stream
/// scrolls (exercising scrollback, the path most at risk of per-line allocation).
fn workload(lines: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..lines {
        v.extend_from_slice(b"\x1b[1;32m");
        for c in 0..72 {
            v.push(b'a' + ((i + c) % 26) as u8);
        }
        v.extend_from_slice(b"\x1b[0m\r\n");
    }
    v
}

/// Like [`workload`] but each row is multi-byte UTF-8 (CJK + emoji), which drives the bulk
/// UTF-8 decode path.
fn unicode_workload(lines: usize) -> Vec<u8> {
    let glyphs = ['中', '文', '日', '本', '한', '😀', 'é', '∑'];
    let mut v = Vec::new();
    let mut buf = [0u8; 4];
    for i in 0..lines {
        for c in 0..36 {
            let g = glyphs[(i + c) % glyphs.len()];
            v.extend_from_slice(g.encode_utf8(&mut buf).as_bytes());
        }
        v.extend_from_slice(b"\r\n");
    }
    v
}

/// Default scrollback is 10,000 lines; warming past it fills the ring so further
/// scrolling reuses slots (the steady state we measure).
const WARM_LINES: usize = 11_000;

/// Allocation calls to process `measure_input` on an engine warmed to scrollback capacity.
fn steady_state_alloc_calls(measure_input: &[u8]) -> u64 {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(&workload(WARM_LINES)); // fill grid + scrollback ring to capacity
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    t.process(measure_input);
    ACTIVE.store(false, Ordering::Relaxed);
    ALLOC_CALLS.load(Ordering::Relaxed)
}

/// Allocation calls made while `f` runs.
fn count_allocations(f: impl FnOnce()) -> u64 {
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    f();
    ACTIVE.store(false, Ordering::Relaxed);
    ALLOC_CALLS.load(Ordering::Relaxed)
}

fn render_extras_avoid_temporary_allocations() {
    use aterm_core::render::RenderInput;
    use aterm_core::terminal::Terminal;
    use aterm_grid::{CellDataView, CellExtra};
    use std::hint::black_box;
    use std::sync::Arc;

    // A history cell can hold a complex string and separately stored combining marks; the
    // borrowed view yields both, in order, without a temporary Vec<char> per read.
    let mut extra = CellExtra::default();
    extra.set_complex_char(Some(Arc::from("👩\u{200D}💻")));
    extra.add_combining('\u{0301}');
    extra.add_combining('\u{0323}');
    let data = CellDataView::History(Some(&extra));
    const TAIL: &str = "\u{200D}💻\u{0301}\u{0323}";
    let mut identical = true;
    let borrowed = count_allocations(|| {
        for _ in 0..128 {
            identical &= black_box(data).marks().eq(TAIL.chars());
        }
    });
    assert!(identical, "the history tail lost or reordered marks");
    // Two allocations of slack for the harness (the counter is process-wide); a per-read
    // Vec, the negative control below, makes at least 128.
    assert!(
        borrowed <= 2,
        "borrowed history marks allocated {borrowed} times"
    );
    let historical = count_allocations(|| {
        for _ in 0..128 {
            let mut marks: Vec<char> = extra.complex_char().unwrap().chars().skip(1).collect();
            marks.extend_from_slice(extra.combining());
            black_box(marks);
        }
    });
    assert!(
        historical >= 128,
        "the allocating negative control must fire"
    );

    // The caller-owned frame buffers are already warm; only the owned cluster
    // strings may allocate, each sized once.
    const CLUSTERS: [&str; 5] = [
        "👨\u{200D}👩\u{200D}👧",
        "👍🏽",
        "1\u{FE0F}\u{20E3}",
        "🇺🇸",
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
    ];
    let mut terminal = Terminal::new(2, 32);
    terminal.process(CLUSTERS.join(" ").as_bytes());
    let mut frame = RenderInput::empty();
    terminal.cell_frame_into(&mut frame, 2, 32);
    assert_eq!(frame.clusters[0].len(), CLUSTERS.len());
    assert!(
        frame.clusters[0]
            .iter()
            .map(|(_, s)| s.as_ref())
            .eq(CLUSTERS)
    );
    let cluster_allocations = count_allocations(|| {
        for _ in 0..16 {
            terminal.cell_frame_into(black_box(&mut frame), 2, 32);
        }
    });
    assert!(
        cluster_allocations <= 16 * CLUSTERS.len() as u64 + 2,
        "warm extraction made {cluster_allocations} allocations for {} required cluster strings",
        16 * CLUSTERS.len()
    );
    let historical_clusters = count_allocations(|| {
        for _ in 0..16 {
            for cluster in CLUSTERS {
                let mut scalars = cluster.chars();
                let base = scalars.next().unwrap();
                let mut text = String::with_capacity(2 + scalars.clone().count());
                text.push(base);
                text.extend(scalars);
                black_box(text.into_boxed_str());
            }
        }
    });
    assert!(
        historical_clusters > cluster_allocations,
        "the scalar-count reservation negative control must allocate more"
    );
}

// Orders of magnitude below the line count: a per-line or per-cell allocation
// (thousands) is caught.
const CEILING: u64 = 64;

// One test: the allocation counter is process-wide, so the measured spans must
// not run on parallel test threads.
#[test]
fn engine_steady_state_processing_is_allocation_free() {
    let measure = 2_000;

    // ASCII: at steady state the ring is full, so scrolling more lines reuses slots.
    let ascii = steady_state_alloc_calls(&workload(measure));
    eprintln!("PERF-BASELINE (ascii):   {measure} lines at steady-state -> {ascii} allocations");
    assert!(
        ascii <= CEILING,
        "steady-state processing of {measure} ascii lines made {ascii} allocations (ceiling \
         {CEILING}) — the engine is allocating per line/cell instead of reusing the ring"
    );

    // CJK/emoji: the grapheme write path allocates grapheme storage, so assert linear
    // scaling instead: 2x the lines must not cost ~4x the allocations.
    let u_n = steady_state_alloc_calls(&unicode_workload(measure));
    let u_2n = steady_state_alloc_calls(&unicode_workload(measure * 2));
    eprintln!(
        "PERF-BASELINE (unicode): {measure} lines -> {u_n} allocs, {} lines -> {u_2n} allocs (ratio {:.2}x)",
        measure * 2,
        u_2n as f64 / u_n.max(1) as f64
    );
    // Linear is 2.0x, quadratic ~4x; 3x leaves headroom for fixed per-call cost.
    assert!(
        u_2n <= u_n.max(1) * 3,
        "unicode allocation scaled super-linearly: {u_n} -> {u_2n} for 2x input (ceiling {}) \
         — the CJK/emoji write path has a quadratic-allocation regression",
        u_n.max(1) * 3
    );

    render_extras_avoid_temporary_allocations();
}
