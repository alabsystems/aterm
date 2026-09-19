// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the kitty-command TRICK FLASH.
//!
//! The flash has two halves on paths that must not allocate. The NOTE runs on
//! the input path at a boundary key (`WordDecorations::note_trick_typed`, with
//! `revoke_trick_flash` and `trick_flash_phase` beside it): its law is O(word
//! length) into fixed inline buffers, whatever the word — Thai under combining
//! marks, the full 32-character window, one character too many, nothing at
//! all. The PAINT runs once per presented frame for about a second: at most 32
//! cells merged into the host's resident `ink` scratch, which has its capacity
//! after the first frame.
//!
//! The rescan half is not pinned here: it lends the engine's resident row
//! buffer, and the scan pass around it has its own allocation story.
//!
//! ONE `#[test]` IN ITS OWN BINARY, like `typed_tricks_alloc.rs`: the counter
//! is a process-global allocator switched by a global flag, so a second test
//! running in parallel would bleed its allocations into the count and make the
//! pin flaky. Do not add tests to this file.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use aterm_core::terminal::Terminal;
use aterm_effects::word_decorations::{DecoConfig, EffectGeom, TrickFlashPhase, WordDecorations};
use aterm_lexicon::{Lexicon, TrickLexicon};
use aterm_time::{Duration, Instant};

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
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

fn allocations_during(f: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Release);
    f();
    COUNT_ALLOCATIONS.store(false, Ordering::Release);
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn a_flash_costs_the_input_path_and_the_frame_path_no_allocation() {
    let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let geom = EffectGeom::default();
    let mut term = Terminal::new(4, 40);
    let mut wd = WordDecorations::default();
    let (mut out, mut ink, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut now = Instant::now();

    // A wordless screen — `sit` is nobody's lexicon word — and a caret on
    // record, without which a note stores nothing at all.
    term.process(b"$ sit");
    wd.rescan(&term, 4, 40, &lex, &cfg, 1, now);

    // Every shape a note can take, built BEFORE the counter is on: the word
    // the glass holds goes LAST, so it is the one left pending.
    let window = "x".repeat(TrickLexicon::MAX_SURFACE_CHARS);
    let too_long = "x".repeat(TrickLexicon::MAX_SURFACE_CHARS + 1);
    let shapes: [&str; 6] = ["", "\u{200d}", "นั่ง", &too_long, &window, "sit"];

    // THE INPUT PATH. No flash has started yet, so no note is debounced away
    // before its character loop: each one really walks its word.
    let mut pending = 0usize;
    let allocations = allocations_during(|| {
        for word in shapes {
            wd.revoke_trick_flash(now);
            wd.note_trick_typed(now, None, word, 0);
            pending += usize::from(wd.trick_flash_phase(now) == TrickFlashPhase::Pending);
        }
    });
    assert_eq!(
        pending, 3,
        "the Thai word, the full window and `sit` are stored"
    );
    assert_eq!(allocations, 0, "a note must not allocate");

    // The Space echoes, the rescan finds the word, and ONE frame warms the
    // host's `ink` scratch. (Neither is counted: see the module docs.)
    term.process(b" ");
    wd.rescan(&term, 4, 40, &lex, &cfg, 2, now);
    assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Live);
    now += Duration::from_millis(16);
    wd.tick(
        now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
    );
    assert_eq!(ink.len(), 3, "one cell per glyph of `sit`");

    // THE FRAME PATH, from the ramp through the hold: every frame repaints
    // all three cells and fingerprints differently.
    let (mut painted, mut changed, mut last_fp) = (0usize, 0usize, 0u64);
    let allocations = allocations_during(|| {
        for _ in 0..40 {
            now += Duration::from_millis(16);
            let fp = wd.tick(
                now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
            painted += ink.len();
            changed += usize::from(fp != last_fp);
            last_fp = fp;
        }
    });
    assert_eq!(
        painted,
        40 * 3,
        "the counted frames really painted the flash"
    );
    assert_eq!(changed, 40, "and every one of them was a new frame");
    assert_eq!(allocations, 0, "a flashing frame must not allocate");

    // A revoke mid-flash is input-path work too, and so is the fade it starts.
    let allocations = allocations_during(|| {
        wd.revoke_trick_flash(now);
        for _ in 0..4 {
            now += Duration::from_millis(16);
            wd.tick(
                now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
        }
    });
    assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Revoked);
    assert_eq!(allocations, 0, "a revoke and its fade must not allocate");
}
