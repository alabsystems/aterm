// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[path = "common/heap_book.rs"]
mod heap_book;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use heap_book::{Book, Busy, FRAMES, MIN_TRACK, Stack};

fn pointer(address: usize) -> *mut u8 {
    std::ptr::without_provenance_mut(address)
}

// The real profiler counts success, then captures a stack only while attribution is not recursive.
fn allocated(book: &Book, ptr: *mut u8, size: usize, frame: usize) {
    book.allocated(ptr, size);
    if let Some(busy) = Busy::enter() {
        let mut frames = [0; FRAMES];
        frames[0] = frame;
        book.attribute(
            ptr,
            size,
            Stack {
                key: frame as u64,
                frames,
                depth: 1,
            },
            &busy,
        );
    }
}

fn owners(book: &Book) -> Vec<(usize, usize, usize)> {
    let state = book.lock();
    let mut owners: Vec<_> = state
        .live
        .iter()
        .map(|(&ptr, record)| {
            let site = &state.sites[record.site as usize];
            assert_eq!(site.depth, 1);
            (ptr, record.size, site.frames[0])
        })
        .collect();
    owners.sort_unstable();
    owners
}

#[test]
fn failed_reallocations_preserve_original_ownership_and_all_counters() {
    let book = Book::new();
    let ptr = pointer(0x1000);
    allocated(&book, ptr, 4_096, 11);
    for size in [128, 16_384] {
        let result = book.reallocated(ptr, 4_096, std::ptr::null_mut);
        allocated(&book, result, size, 22);
        assert!(result.is_null());
        assert_eq!(book.live_bytes.load(Relaxed), 4_096);
        assert_eq!(book.live_count.load(Relaxed), 1);
        assert_eq!(book.peak_bytes.load(Relaxed), 4_096);
        assert_eq!(book.total_allocs.load(Relaxed), 1);
        assert_eq!(owners(&book), [(0x1000, 4_096, 11)]);
        let state = book.lock();
        assert_eq!(state.allocation_count(), 1);
        assert_eq!(state.sites.len(), 1);
        assert_eq!(state.sites[0].live, 4_096);
        assert_eq!(state.sites[0].live_count, 1);
        assert_eq!(state.sites[0].total, 4_096);
    }
    book.deallocated(ptr, 4_096);
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.live_count.load(Relaxed), 0);
    assert_eq!(book.lock().allocation_count(), 0);
}

#[test]
fn a_new_owner_can_reuse_the_old_address_before_realloc_returns() {
    let book = Book::new();
    allocated(&book, pointer(0x1000), 4_096, 11);
    let result = std::thread::scope(|threads| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let book = &book;
        book.reallocated(pointer(0x1000), 4_096, || {
            threads.spawn(move || {
                // Native realloc has released the old address. This is a different allocation,
                // observed by a different thread before the first thread can commit its result.
                allocated(book, pointer(0x1000), 1_024, 22);
                let count = book.lock().allocation_count();
                let _ = tx.send(count);
            });
            let count = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("native operation holds no book lock");
            assert_eq!(count, 2, "old owner is still pending");
            pointer(0x2000)
        })
    });
    allocated(&book, result, 2_048, 33);
    assert_eq!(result, pointer(0x2000));
    assert_eq!(owners(&book), [(0x1000, 1_024, 22), (0x2000, 2_048, 33)]);
    assert_eq!(book.live_bytes.load(Relaxed), 3_072);
    assert_eq!(book.live_count.load(Relaxed), 2);
    assert_eq!(book.total_allocs.load(Relaxed), 3);
    {
        let state = book.lock();
        assert_eq!(state.allocation_count(), 2);
        assert_eq!(state.sites[0].live, 0, "the captured owner was retired");
        assert_eq!(state.sites[0].live_count, 0);
        assert_eq!(state.sites[1].live, 1_024, "the new owner was preserved");
        assert_eq!(state.sites[2].live, 2_048);
    }
    book.deallocated(pointer(0x1000), 1_024);
    book.deallocated(result, 2_048);
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.lock().allocation_count(), 0);
}

#[test]
fn in_place_growth_shrink_and_tracking_boundary_keep_one_owner() {
    let book = Book::new();
    let ptr = pointer(0x1000);
    allocated(&book, ptr, 4_096, 11);
    let mut old = 4_096;
    for (size, frame) in [(8_192, 22), (MIN_TRACK, 33), (128, 44), (MIN_TRACK, 55)] {
        let result = book.reallocated(ptr, old, || ptr);
        assert_eq!(result, ptr);
        allocated(&book, result, size, frame);
        assert_eq!(book.live_bytes.load(Relaxed), size as i64);
        assert_eq!(book.live_count.load(Relaxed), 1);
        if size >= MIN_TRACK {
            assert_eq!(owners(&book), [(0x1000, size, frame)]);
            assert_eq!(book.lock().allocation_count(), 1);
        } else {
            assert!(owners(&book).is_empty());
            assert_eq!(book.lock().allocation_count(), 0);
        }
        old = size;
    }
    assert_eq!(book.peak_bytes.load(Relaxed), 8_192);
    assert_eq!(book.total_allocs.load(Relaxed), 5);
    book.deallocated(ptr, old);
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.live_count.load(Relaxed), 0);
    assert!(
        book.lock()
            .sites
            .iter()
            .all(|site| site.live == 0 && site.live_count == 0)
    );
}

#[test]
fn nested_bookkeeping_counts_bytes_without_recursing_into_attribution() {
    let book = Book::new();
    {
        let _busy = Busy::force();
        assert!(Busy::enter().is_none());
        allocated(&book, pointer(0x1000), 1_024, 11);
        let result = book.reallocated(pointer(0x1000), 1_024, || pointer(0x2000));
        allocated(&book, result, 2_048, 22);
        assert_eq!(book.live_bytes.load(Relaxed), 2_048);
        assert!(book.lock().live.is_empty());
        book.deallocated(result, 2_048);
    }
    assert!(
        Busy::enter().is_some(),
        "the recursive scope restores the TLS flag"
    );
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.live_count.load(Relaxed), 0);
    assert_eq!(book.lock().allocation_count(), 0);
}

#[test]
fn nested_suppression_survives_out_of_order_guard_drops() {
    let book = Book::new();
    let outer = Busy::force();
    let inner = Busy::force();
    drop(outer);
    allocated(&book, pointer(0x1000), 1_024, 11);
    assert!(
        book.lock().live.is_empty(),
        "the inner guard still suppresses attribution"
    );
    book.deallocated(pointer(0x1000), 1_024);
    drop(inner);
    allocated(&book, pointer(0x2000), 1_024, 22);
    assert_eq!(owners(&book), [(0x2000, 1_024, 22)]);
    book.deallocated(pointer(0x2000), 1_024);
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.lock().allocation_count(), 0);
}

#[test]
fn unwinding_the_native_callback_restores_its_detached_record() {
    let book = Book::new();
    allocated(&book, pointer(0x1000), 1_024, 11);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        book.reallocated(pointer(0x1000), 1_024, || {
            panic!("before allocator mutation")
        });
    }));
    assert!(result.is_err());
    assert_eq!(owners(&book), [(0x1000, 1_024, 11)]);
    assert_eq!(book.live_bytes.load(Relaxed), 1_024);
    assert_eq!(book.live_count.load(Relaxed), 1);
    assert_eq!(book.lock().allocation_count(), 1);
    book.deallocated(pointer(0x1000), 1_024);
}

#[test]
fn native_system_resize_preserves_contents_and_balances_the_book() {
    let book = Book::new();
    let layout = Layout::from_size_align(1_024, 16).unwrap();
    let grown = Layout::from_size_align(8_192, 16).unwrap();
    // SAFETY: the System block is used within its layout, resized with that layout, and freed
    // exactly once. The refusal callback deliberately leaves the real original block untouched.
    unsafe {
        let ptr = System.alloc_zeroed(layout);
        assert!(!ptr.is_null());
        allocated(&book, ptr, 1_024, 11);
        assert!(
            std::slice::from_raw_parts(ptr, 1_024)
                .iter()
                .all(|b| *b == 0)
        );
        ptr.write_bytes(0xa5, 1_024);
        assert!(book.reallocated(ptr, 1_024, std::ptr::null_mut).is_null());
        let next = book.reallocated(ptr, 1_024, || System.realloc(ptr, layout, 8_192));
        assert!(!next.is_null());
        allocated(&book, next, 8_192, 22);
        assert!(
            std::slice::from_raw_parts(next, 1_024)
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert_eq!(book.live_bytes.load(Relaxed), 8_192);
        assert_eq!(book.live_count.load(Relaxed), 1);
        book.deallocated(next, 8_192);
        System.dealloc(next, grown);
    }
    assert_eq!(book.live_bytes.load(Relaxed), 0);
    assert_eq!(book.live_count.load(Relaxed), 0);
    assert_eq!(book.lock().allocation_count(), 0);
}
