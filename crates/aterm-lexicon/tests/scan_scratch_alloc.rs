// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the caller-owned scanner workspace.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use aterm_lexicon::{Lexicon, Match, ScanOptions, ScanScratch};

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
fn warmed_scan_scratch_is_allocation_free_across_every_scanner_lane() {
    let lexicon = Lexicon::with_languages(&["all"]);
    let options = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ignore: None,
    };
    // Ordinary and possessive spaced tokens, a CJK maximal run, Arabic
    // definite-article + one-letter clitic fallbacks, and five result entries.
    let text = "kitty's 子猫 القطة وقطة fuck";
    let mut chars = Vec::new();
    let mut hits: Vec<Match> = Vec::new();
    let mut scratch = ScanScratch::default();

    lexicon.scan_into_with_scratch(text, &options, &mut chars, &mut hits, &mut scratch);
    assert_eq!(hits.len(), 5, "warmup must exercise every scanner lane");

    let allocations = allocations_during(|| {
        lexicon.scan_into_with_scratch(text, &options, &mut chars, &mut hits, &mut scratch);
    });
    assert_eq!(allocations, 0, "warmed scratch scan must not allocate");
}
