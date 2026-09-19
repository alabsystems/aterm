// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the kitty-command lookup.
//!
//! `TrickLexicon::classify` runs on the typing path — once per word boundary
//! the person types — so after its caller-owned fold buffer has warmed it must
//! allocate NOTHING, on every lane: a plain hit, a marked surface, a raw
//! no-space-script run, the elongation fold's two retries, and a miss.
//!
//! ONE `#[test]` IN ITS OWN BINARY, like `scan_scratch_alloc.rs`: the counter
//! is a process-global allocator switched by a global flag, so a second test
//! running in parallel would bleed its allocations into the count and make the
//! pin flaky. Do not add tests to this file.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use aterm_lexicon::{Trick, TrickLexicon, TrickRole};

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

/// A vocabulary that exercises the two lanes the English seed cannot: a
/// MARKED surface (the marks-required gate walks the token) and a raw
/// no-space-script run. English rides the embedded file.
const LANES: &str = r#"
[[trick]]
id    = "sit"
lang  = "es"
words = ["siéntate"]

[[trick]]
id    = "sit"
lang  = "ja"
words = ["おすわり"]
cjk   = true
"#;

#[test]
fn a_warmed_classify_is_allocation_free_on_every_lane() {
    let en = TrickLexicon::shared_en();
    let lanes = TrickLexicon::from_source(LANES, &["all"]).expect("the lane vocabulary parses");
    assert!(lanes.conflicts().is_empty(), "{:?}", lanes.conflicts());

    let plain_sit = Some(TrickRole::Trick {
        trick: Trick::Sit,
        needs_vocative: false,
    });
    // (table, token, expected): every lane, hits and misses alike. The long
    // miss is the widest thing the fold buffer ever has to hold, so it is
    // also what the warmup pass sizes the buffer with.
    let cases: [(&TrickLexicon, &str, Option<TrickRole>); 9] = [
        (en, "sit", plain_sit),
        (en, "SIT", plain_sit),
        (en, "kitty", Some(TrickRole::Vocative)),
        (en, "siiiiiiiit", plain_sit),
        (
            en,
            "nooooooooo",
            Some(TrickRole::Trick {
                trick: Trick::Scold,
                needs_vocative: true,
            }),
        ),
        (&lanes, "siéntate", plain_sit),
        (&lanes, "sientate", None),
        (&lanes, "おすわり", plain_sit),
        (
            en,
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            None,
        ),
    ];

    let mut scratch = String::new();
    // Warmup: grow the caller's buffer to the widest token, and check every
    // lane really does what the comment says (a lane that silently missed
    // would make the count below vacuous).
    for (table, token, expected) in cases {
        assert_eq!(table.classify(token, &mut scratch), expected, "{token:?}");
    }

    let allocations = allocations_during(|| {
        for (table, token, expected) in cases {
            // `assert_eq!` would format on failure only; the comparison
            // itself allocates nothing.
            assert!(table.classify(token, &mut scratch) == expected);
        }
    });
    assert_eq!(allocations, 0, "a warmed classify must not allocate");
}
