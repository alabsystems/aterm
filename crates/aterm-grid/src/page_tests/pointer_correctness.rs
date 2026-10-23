// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

#[test]
fn alloc_slice_alignment_u64() {
    let mut store = PageStore::new();
    let _byte = store.alloc_slice::<u8>(1);
    let slice = store.alloc_slice::<u64>(10);
    assert_eq!(slice.len(), 10);
    let ptr = slice.as_slice().as_ptr();
    assert_eq!(ptr.align_offset(std::mem::align_of::<u64>()), 0);
}

#[test]
fn alloc_slice_alignment_u128() {
    let mut store = PageStore::new();
    let _byte = store.alloc_slice::<u8>(3); // misalign
    let slice = store.alloc_slice::<u128>(5);
    assert_eq!(slice.len(), 5);
    let ptr = slice.as_slice().as_ptr();
    assert_eq!(ptr.align_offset(std::mem::align_of::<u128>()), 0);
}

#[test]
fn alloc_zero_length_slice_is_stable_and_non_consuming() {
    let mut store = PageStore::new();

    let mut empty = store.alloc_slice::<u32>(0);
    assert_eq!(empty.len(), 0);
    assert!(empty.as_slice().is_empty());
    assert!(empty.as_mut_slice().is_empty());
    let empty_ptr = empty.as_slice().as_ptr();
    let empty_mut_ptr = empty.as_mut_slice().as_mut_ptr();
    assert_eq!(empty_ptr, empty_mut_ptr);
    let empty_offset = empty.offset().byte_offset();
    let empty_page_id = empty.page_id();

    let mut filled = store.alloc_slice::<u32>(4);
    assert_eq!(filled.page_id(), empty_page_id);
    assert_eq!(
        filled.offset().byte_offset(),
        empty_offset,
        "len=0 allocation must not advance page offset"
    );
    for (i, value) in filled.iter_mut().enumerate() {
        *value = i as u32 + 1;
    }
    assert_eq!(filled.as_slice(), &[1, 2, 3, 4]);
}

#[test]
fn alloc_slice_pointer_stability_across_allocations() {
    let mut store = PageStore::new();

    let mut s1 = store.alloc_slice::<u32>(100);
    let mut s2 = store.alloc_slice::<u32>(100);
    let mut s3 = store.alloc_slice::<u32>(100);

    for (i, val) in s1.iter_mut().enumerate() {
        *val = 0xAAAA_0000 + i as u32;
    }
    for (i, val) in s2.iter_mut().enumerate() {
        *val = 0xBBBB_0000 + i as u32;
    }
    for (i, val) in s3.iter_mut().enumerate() {
        *val = 0xCCCC_0000 + i as u32;
    }

    for _ in 0..10 {
        let _extra = store.alloc_slice::<u32>(1000);
    }

    for (i, &val) in s1.iter().enumerate() {
        assert_eq!(
            val,
            0xAAAA_0000 + i as u32,
            "s1[{i}] corrupted after subsequent allocations"
        );
    }
    for (i, &val) in s2.iter().enumerate() {
        assert_eq!(
            val,
            0xBBBB_0000 + i as u32,
            "s2[{i}] corrupted after subsequent allocations"
        );
    }
    for (i, &val) in s3.iter().enumerate() {
        assert_eq!(
            val,
            0xCCCC_0000 + i as u32,
            "s3[{i}] corrupted after subsequent allocations"
        );
    }
}

#[test]
fn alloc_slice_pointer_stability_across_pages() {
    use crate::Cell;

    let mut store = PageStore::new();

    let mut first_page_cells = store.alloc_slice::<Cell>(8000);
    for cell in first_page_cells.iter_mut() {
        cell.set_char('X');
    }

    let mut second_page_cells = store.alloc_slice::<Cell>(8000);
    for cell in second_page_cells.iter_mut() {
        cell.set_char('Y');
    }

    assert_eq!(store.active_pages(), 2, "expected exactly 2 active pages");

    for (i, cell) in first_page_cells.as_slice().iter().enumerate() {
        assert_eq!(
            cell.char(),
            'X',
            "first page cell[{i}] corrupted after second page allocation"
        );
    }
    for (i, cell) in second_page_cells.as_slice().iter().enumerate() {
        assert_eq!(cell.char(), 'Y', "second page cell[{i}] corrupted");
    }
}
