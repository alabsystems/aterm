// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PAGE, RegionStore, Touched, VecStore};

#[test]
fn written_page_ranges_survive_zeroing_and_clip_untrusted_bounds() {
    let store = Touched::new(VecStore::new(130 * PAGE + 1), None);
    assert_eq!(store.touched_bytes_in(0, usize::MAX), 0);
    assert!(!store.touches(0, usize::MAX));
    for page in [0, 63, 64, 129, 130] {
        store
            .write(page * PAGE, &[9])
            .expect("one byte in the page");
    }
    assert_eq!(store.touched_bytes(), 5 * PAGE);
    assert_eq!(store.touched_bytes_in(0, usize::MAX), 5 * PAGE);
    assert!(store.touches(0, usize::MAX));
    assert_eq!(store.touched_bytes_in(63 * PAGE, 2 * PAGE), 2 * PAGE);
    assert_eq!(store.touched_bytes_in(64 * PAGE - 1, 2), 2 * PAGE);
    assert_eq!(store.touched_bytes_in(PAGE, 62 * PAGE), 0);
    assert!(!store.touches(PAGE, 62 * PAGE));
    assert_eq!(store.touched_bytes_in(130 * PAGE, usize::MAX), PAGE);
    assert!(store.touches(130 * PAGE, usize::MAX));
    assert!(!store.touches(130 * PAGE + 1, usize::MAX));
    assert_eq!(store.touched_bytes_in(0, 0), 0);
    assert!(!store.touches(0, 0));
    assert_eq!(store.touched_bytes_in(usize::MAX, usize::MAX), 0);
    assert!(!store.touches(usize::MAX, usize::MAX));
    for page in [0, 63, 64, 129, 130] {
        store.write(page * PAGE, &[0]).expect("zero the old byte");
    }
    assert_eq!(store.touched_bytes_in(0, usize::MAX), 5 * PAGE);
}
