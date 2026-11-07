// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compare sparse extras' retained-memory estimate with allocator observations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATED: AtomicU64 = AtomicU64::new(0);
static FREED: AtomicU64 = AtomicU64::new(0);
struct Counting;

// SAFETY: all pointers and layouts pass through unchanged to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: forwarding the caller's live allocation and layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwarding the caller's live allocation, layout and new size.
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
            FREED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        next
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn live() -> u64 {
    ALLOCATED.load(Ordering::Relaxed) - FREED.load(Ordering::Relaxed)
}

#[test]
fn sparse_extras_estimate_covers_retained_table_allocations() {
    sparse_erase_shrinks_retained_buckets();
    let mut failures = 0;
    for entries in [1_u16, 8, 64, 1024, 8192] {
        for state in ["full", "half", "quarter", "churn", "clone_half", "clear"] {
            let mut grid = aterm_grid::Grid::new(1, 8192);
            let baseline = grid.memory_used();
            let allocated = live();
            for col in 0..entries {
                grid.extras_mut()
                    .get_or_create(aterm_grid::CellCoord::new(0, col))
                    .set_underline_color_u32(Some(0x02000007));
            }
            if state != "full" {
                grid.extras_mut().clear_range(0, 0, entries / 2);
            }
            if state == "quarter" {
                grid.extras_mut()
                    .clear_range(0, entries / 2, entries - entries / 4);
            }
            if state == "churn" {
                for _ in 0..8 {
                    for col in 0..entries / 2 {
                        grid.extras_mut()
                            .get_or_create(aterm_grid::CellCoord::new(0, col))
                            .set_underline_color_u32(Some(0x02000007));
                    }
                    grid.extras_mut().clear_range(0, 0, entries / 2);
                }
            }
            if state == "clone_half" {
                let cloned = grid.extras().clone();
                *grid.extras_mut() = cloned;
            }
            if state == "clear" {
                grid.extras_mut().clear();
            }
            let retained = live() - allocated;
            let reported = grid.memory_used() - baseline;
            let expected = match state {
                "full" => entries,
                "quarter" => entries / 4,
                "clear" => 0,
                _ => entries - entries / 2,
            };
            assert_eq!(grid.extras().len(), usize::from(expected));
            if expected != 0 {
                assert_eq!(
                    grid.cell_extra(0, entries - 1)
                        .unwrap()
                        .underline_color_u32(),
                    Some(0x02000007)
                );
            }
            println!(
                "extras_memory entries={entries} state={state} retained_bytes={retained} reported_bytes={reported}"
            );
            if (reported as u64) < retained || (reported as u64) > retained + 32 {
                failures += 1;
            }
        }
    }
    assert_eq!(
        failures, 0,
        "sparse table estimates must cover retained storage within two control groups"
    );
}

fn sparse_erase_shrinks_retained_buckets() {
    let mut excessive = Vec::new();
    for entries in [112_u16, 896, 7168, 14336] {
        let keep = entries / 4 - 1;
        let mut grid = aterm_grid::Grid::new(1, 16384);
        let mut reference = aterm_grid::Grid::new(1, 16384);
        let reported_baseline = grid.memory_used();
        let baseline = live();
        for col in 0..entries {
            grid.extras_mut()
                .get_or_create(aterm_grid::CellCoord::new(0, col))
                .set_underline_color_u32(Some(0x02000007));
        }
        grid.extras_mut().clear_range(0, 0, entries - keep);
        let retained = live() - baseline;
        let reported = grid.memory_used() - reported_baseline;
        assert!((reported as u64) >= retained && (reported as u64) <= retained + 32);
        for col in entries - keep..entries {
            reference
                .extras_mut()
                .get_or_create(aterm_grid::CellCoord::new(0, col))
                .set_underline_color_u32(Some(0x02000007));
        }
        let fresh = live() - baseline - retained;
        assert_eq!(grid.extras().len(), usize::from(keep));
        for col in entries - keep..entries {
            assert_eq!(
                grid.cell_extra(0, col).unwrap().underline_color_u32(),
                Some(0x02000007)
            );
        }
        println!(
            "extras_shrink entries={entries} keep={keep} retained_bytes={retained} fresh_bytes={fresh}"
        );
        if retained > fresh {
            excessive.push((entries, keep, retained, fresh));
        }
    }
    assert!(
        excessive.is_empty(),
        "erased tables exceeded the shrinking threshold: {excessive:?}"
    );
}
