// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

// Send/Sync impls: concurrent reads through shared references, checked under
// Miri's Stacked Borrows (moving an Arc into a thread also needs Send).

/// Several threads read one `Arc<Page>` concurrently (`unsafe impl Sync for Page`).
#[test]
fn page_sync_concurrent_readers() {
    use std::sync::Arc;

    let page = Arc::new(Page::new());

    let p1 = Arc::clone(&page);
    let h1 = std::thread::spawn(move || {
        let ptr = p1.data_ptr();
        assert!(!ptr.is_null());
        let val = unsafe { *ptr };
        assert_eq!(val, 0);
    });

    let p2 = Arc::clone(&page);
    let h2 = std::thread::spawn(move || {
        let ptr = p2.data_ptr();
        assert!(!ptr.is_null());
        let val = unsafe { *ptr.add(PAGE_SIZE / 2) };
        assert_eq!(val, 0);
    });

    h1.join().expect("reader 1 panicked");
    h2.join().expect("reader 2 panicked");
}

/// Several threads read one `Arc<PageSlice<u32>>` concurrently
/// (`unsafe impl Sync for PageSlice<T>`); `as_slice()` goes through
/// `from_raw_parts`.
#[test]
fn page_slice_sync_concurrent_readers() {
    use std::sync::Arc;

    let mut store = PageStore::new();
    let mut slice = store.alloc_slice::<u32>(100);

    for (i, val) in slice.iter_mut().enumerate() {
        *val = (i as u32) * 13 + 7;
    }

    // PageSlice<u32> is Sync because u32 is.
    let shared = Arc::new(slice);

    let s1 = Arc::clone(&shared);
    let h1 = std::thread::spawn(move || {
        for (i, &val) in s1.as_slice()[..50].iter().enumerate() {
            assert_eq!(val, (i as u32) * 13 + 7, "reader1 s[{i}] wrong");
        }
    });

    let s2 = Arc::clone(&shared);
    let h2 = std::thread::spawn(move || {
        for (i, &val) in s2.as_slice()[50..].iter().enumerate() {
            let idx = i + 50;
            assert_eq!(val, (idx as u32) * 13 + 7, "reader2 s[{idx}] wrong");
        }
    });

    h1.join().expect("reader 1 panicked");
    h2.join().expect("reader 2 panicked");
}
