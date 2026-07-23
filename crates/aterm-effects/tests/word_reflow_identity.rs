// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public-API regression for same-width full-redraw identity transfer.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use aterm_core::terminal::Terminal;
use aterm_effects::word_decorations::{DecoConfig, EffectGeom, ProfanityStyle, WordDecorations};
use aterm_lexicon::Lexicon;
use aterm_render::{GlowQuad, InkCell, RenderInput, WordDecoration};

const ROWS: usize = 8;
const COLS: usize = 48;

/// This integration-test binary has one test, so a process-wide counter gives
/// a deterministic public-API warm-path allocation gate without interference
/// from sibling tests.
struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_SIZES: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];

fn record_allocation(size: usize) {
    if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
        let index = ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = ALLOCATION_SIZES.get(index) {
            slot.store(size, Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record_allocation(new_size);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this delegating allocator.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(f: impl FnOnce()) -> (usize, [usize; 16]) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    for slot in &ALLOCATION_SIZES {
        slot.store(0, Ordering::Relaxed);
    }
    COUNT_ALLOCATIONS.store(true, Ordering::Release);
    f();
    COUNT_ALLOCATIONS.store(false, Ordering::Release);
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        std::array::from_fn(|index| ALLOCATION_SIZES[index].load(Ordering::Relaxed)),
    )
}

fn snapshot(lines: &[(usize, &str)]) -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for &(row, text) in lines {
        term.process(format!("\x1b[{};1H{text}", row + 1).as_bytes());
    }
    let mut out = RenderInput::default();
    term.cell_frame_into(&mut out, ROWS, COLS);
    out
}

struct Frame {
    fp: u64,
    decorations: Vec<WordDecoration>,
    ink: Vec<InkCell>,
    nova: Vec<GlowQuad>,
}

fn tick(engine: &mut WordDecorations, cfg: &DecoConfig, now: Instant) -> Frame {
    let mut decorations = Vec::new();
    let mut ink = Vec::new();
    let mut free = Vec::new();
    let mut nova = Vec::new();
    let fp = engine.tick(
        now,
        cfg,
        EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: ROWS as u16,
            cols: COLS as u16,
        },
        None,
        None,
        true,
        &mut decorations,
        &mut ink,
        &mut free,
        &mut nova,
    );
    Frame {
        fp,
        decorations,
        ink,
        nova,
    }
}

fn rescan(
    engine: &mut WordDecorations,
    lexicon: &Lexicon,
    cfg: &DecoConfig,
    cells: &RenderInput,
    epoch: u64,
    now: Instant,
) {
    engine.rescan_from_cells_with_geom(
        &cells.cells,
        &cells.line_sizes,
        ROWS,
        COLS,
        lexicon,
        cfg,
        epoch,
        now,
        EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: ROWS as u16,
            cols: COLS as u16,
        },
        cells.default_bg,
    );
}

#[test]
fn spent_profanity_horizontal_redraw_does_not_rearm_or_decorate_fix() {
    let lexicon = Lexicon::with_languages(&["en"]);
    let cfg = DecoConfig {
        profanity_style: ProfanityStyle::Rainbow,
        // Non-vacuity: every genuinely fresh profanity episode must burst.
        supernova_chance: 100,
        ..DecoConfig::default()
    };
    let t0 = Instant::now();
    let mut engine = WordDecorations::default();

    // The original profanity occupies row 1, columns 4..=7.
    let original = snapshot(&[(1, "old fuck remains visible")]);
    rescan(&mut engine, &lexicon, &cfg, &original, 1, t0);
    let born = tick(&mut engine, &cfg, t0);
    assert!(
        !born.nova.is_empty(),
        "positive control: first occurrence bursts"
    );

    // Spend the complete 2400 ms supernova before the redraw under test.
    let settled_at = t0 + Duration::from_millis(3000);
    let settled = tick(&mut engine, &cfg, settled_at);
    assert!(settled.nova.is_empty(), "the original burst is fully spent");

    // A Codex-style full-row redraw, at the SAME terminal width, moves the old
    // sentence down one row and right one column while the composer shows the
    // nonmatching word `fix`. The moved surface/context is one logical episode.
    let redrawn = snapshot(&[(2, " old fuck remains visible"), (5, "fix")]);
    let moved_at = t0 + Duration::from_millis(3100);
    rescan(&mut engine, &lexicon, &cfg, &redrawn, 2, moved_at);
    let moved = tick(&mut engine, &cfg, moved_at);

    assert!(
        moved.nova.is_empty(),
        "a horizontal rekey must preserve the spent burst guard"
    );
    assert_eq!(
        moved.ink.iter().map(|i| (i.row, i.col)).collect::<Vec<_>>(),
        vec![(2, 5), (2, 6), (2, 7), (2, 8)],
        "only the moved profanity owns rainbow ink"
    );
    assert!(
        moved.decorations.iter().all(|d| d.row != 5),
        "the visible `fix` row owns no word-reaction decoration"
    );

    // Warm both directions of the horizontal transfer, then measure a same-row
    // scan and a rekey scan over the same two nonempty rows. Every scanner and
    // identity buffer is resident: both paths must allocate absolutely nothing.
    let redraw_back = snapshot(&[(1, "old fuck remains visible"), (5, "fix")]);
    rescan(
        &mut engine,
        &lexicon,
        &cfg,
        &redraw_back,
        3,
        moved_at + Duration::from_millis(10),
    );
    rescan(
        &mut engine,
        &lexicon,
        &cfg,
        &redrawn,
        4,
        moved_at + Duration::from_millis(20),
    );
    rescan(
        &mut engine,
        &lexicon,
        &cfg,
        &redraw_back,
        5,
        moved_at + Duration::from_millis(30),
    );
    let (scanner_baseline_allocations, scanner_allocation_sizes) = allocations_during(|| {
        rescan(
            &mut engine,
            &lexicon,
            &cfg,
            &redraw_back,
            6,
            moved_at + Duration::from_millis(40),
        );
    });
    let (warm_rekey_allocations, allocation_sizes) = allocations_during(|| {
        rescan(
            &mut engine,
            &lexicon,
            &cfg,
            &redrawn,
            7,
            moved_at + Duration::from_millis(50),
        );
    });
    assert_eq!(
        scanner_baseline_allocations, 0,
        "same-row warm rescan must be allocation-free; sizes={scanner_allocation_sizes:?}"
    );
    assert_eq!(
        warm_rekey_allocations, 0,
        "same-width warm rekey rescan must be allocation-free; sizes={allocation_sizes:?}"
    );

    // Exact lexical negative control: with no profanity elsewhere, `fix` is the
    // byte-identical off path through the public API.
    let mut fix_only = WordDecorations::default();
    let fix = snapshot(&[(5, "fix")]);
    rescan(&mut fix_only, &lexicon, &cfg, &fix, 1, t0);
    let no_match = tick(&mut fix_only, &cfg, t0);
    assert_eq!(no_match.fp, 0);
    assert!(no_match.decorations.is_empty() && no_match.ink.is_empty() && no_match.nova.is_empty());

    // Preserve intentional new-occurrence behavior: a distinct profanity in a
    // different sentence is not the moved episode and therefore still bursts.
    let with_new_occurrence = snapshot(&[
        (2, " old fuck remains visible"),
        (5, "fix"),
        (6, "a newly typed fuck appears"),
    ]);
    let retyped_at = t0 + Duration::from_millis(3200);
    rescan(
        &mut engine,
        &lexicon,
        &cfg,
        &with_new_occurrence,
        8,
        retyped_at,
    );
    let retyped = tick(&mut engine, &cfg, retyped_at);
    assert!(
        !retyped.nova.is_empty(),
        "positive control: a genuinely new occurrence still bursts"
    );

    // Adversarial neighbor/status churn: every ±4 context voter changes, the
    // exact profanity moves down one row and eight columns, and `fix` appears
    // on that same physical row. This is the real Codex full-redraw witness:
    // lexical matching is clean, but treating the old profanity as a fresh
    // episode would make it look as though `fix` caused the flash.
    let mut churn_engine = WordDecorations::default();
    let before_churn = snapshot(&[(1, "one two three fuck four five six")]);
    rescan(&mut churn_engine, &lexicon, &cfg, &before_churn, 1, t0);
    assert!(!tick(&mut churn_engine, &cfg, t0).nova.is_empty());
    let churn_settled = t0 + Duration::from_millis(3000);
    assert!(tick(&mut churn_engine, &cfg, churn_settled).nova.is_empty());
    let after_churn = snapshot(&[(2, "STATUS fix alpha beta fuck gamma delta")]);
    let churned_at = t0 + Duration::from_millis(3100);
    rescan(
        &mut churn_engine,
        &lexicon,
        &cfg,
        &after_churn,
        2,
        churned_at,
    );
    let churned = tick(&mut churn_engine, &cfg, churned_at);
    assert!(
        churned.nova.is_empty(),
        "changed neighbors must not turn the moved spent episode into a fresh burst"
    );
    assert_eq!(
        churned
            .ink
            .iter()
            .map(|i| (i.row, i.col))
            .collect::<Vec<_>>(),
        vec![(2, 22), (2, 23), (2, 24), (2, 25)],
        "only the exact `fuck` surface owns ink; the nearby `fix` owns none"
    );

    // Fixed-width soft-wrap witness: reading-order distance is small even
    // though Manhattan distance is large (high column -> next row low column),
    // and the row-local context is deliberately unrelated.
    let mut wrap_engine = WordDecorations::default();
    let before_wrap = snapshot(&[(1, "                                        fuck old")]);
    rescan(&mut wrap_engine, &lexicon, &cfg, &before_wrap, 1, t0);
    assert!(!tick(&mut wrap_engine, &cfg, t0).nova.is_empty());
    assert!(
        tick(&mut wrap_engine, &cfg, t0 + Duration::from_millis(3000))
            .nova
            .is_empty()
    );
    let after_wrap = snapshot(&[(2, "  fuck new status"), (5, "fix")]);
    let wrapped_at = t0 + Duration::from_millis(3100);
    rescan(&mut wrap_engine, &lexicon, &cfg, &after_wrap, 2, wrapped_at);
    let wrapped = tick(&mut wrap_engine, &cfg, wrapped_at);
    assert!(
        wrapped.nova.is_empty(),
        "soft-wrap + changed context must preserve the spent episode"
    );
    assert_eq!(
        wrapped
            .ink
            .iter()
            .map(|i| (i.row, i.col))
            .collect::<Vec<_>>(),
        vec![(2, 2), (2, 3), (2, 4), (2, 5)],
        "wrapped profanity ink stays on the profanity and never on `fix`"
    );

    let wrap_plus_new = snapshot(&[
        (2, "  fuck new status"),
        (5, "fix"),
        (6, "a genuinely new fuck appears"),
    ]);
    let wrap_new_at = t0 + Duration::from_millis(3200);
    rescan(
        &mut wrap_engine,
        &lexicon,
        &cfg,
        &wrap_plus_new,
        3,
        wrap_new_at,
    );
    assert!(
        !tick(&mut wrap_engine, &cfg, wrap_new_at).nova.is_empty(),
        "positive control: net-new exact profanity remains armed"
    );
}
