// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual requested heap bytes for relation construction and grounded propagation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: allocation and deallocation forward the caller's unchanged layout and pointer to
// System. The constant-initialized thread-local counters allocate nothing. The default realloc
// allocates the new block before freeing the old one, counting both during growth.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's valid layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = ACTIVE.try_with(|active| {
                if active.get() {
                    let _ = LIVE.try_with(|live| {
                        let next = live.get() + layout.size();
                        live.set(next);
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(next)));
                    });
                }
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = LIVE.try_with(|live| live.set(live.get() - layout.size()));
            }
        });
        // SAFETY: forwarding the caller's allocated pointer and matching layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measuring;

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

fn peak_in(run: impl FnOnce() -> usize) -> (usize, usize) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| active.set(true));
    let measuring = Measuring;
    let result = run();
    drop(measuring);
    assert_eq!(
        LIVE.with(Cell::get),
        0,
        "the probe releases its complete working set"
    );
    (result, PEAK.with(Cell::get))
}

#[test]
fn maximum_quorum_relations_keep_empty_storage_linear_and_dense_storage_bounded() {
    let candidates = (aterm_secrets::membership_log::MAX_WEIGHED_RECORDS - 1) / 2;
    // This covers candidate IDs, reverse mapping, row descriptors, counters, labels and
    // worklist growth. The fixed allowance covers vector and iterator bookkeeping; neither
    // allowance grows with the number of relation edges.
    let linear = candidates * 128 + 4096;
    let (empty_result, empty_peak) =
        peak_in(|| aterm_secrets::membership_log::quorum_relations_allocation_probe(false));
    assert_eq!(empty_result, candidates);
    assert!(
        empty_peak <= linear,
        "empty relation allocated {empty_peak} bytes, beyond its {linear}-byte linear allowance"
    );

    let (dense_result, dense_peak) =
        peak_in(|| aterm_secrets::membership_log::quorum_relations_allocation_probe(true));
    assert_eq!(dense_result, 1);
    let dense_rows = (2 * candidates - 1) * candidates.div_ceil(64) * 8;
    assert!(
        dense_peak >= dense_rows,
        "the allocator observed the packed rows"
    );
    let bound = 1024 * 1024 + linear;
    assert!(
        dense_peak <= bound,
        "dense relation allocated {dense_peak} bytes, beyond its {bound}-byte bound"
    );
    // The dense order has M*(M-1)/2 defeating edges. A duplicate usize adjacency alone
    // exceeds this entire allowance by more than a factor of four even with 32-bit indices.
    assert!(candidates * (candidates - 1) / 2 * size_of::<usize>() > 4 * bound);
    println!(
        "quorum relation requested heap: candidates={candidates} empty_peak={empty_peak} dense_peak={dense_peak} bound={bound}"
    );
}
